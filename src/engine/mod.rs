pub mod baseline;
#[allow(dead_code)] // used from the wizard UI on
pub mod calibrate;
pub mod features;
pub mod vad;

use baseline::RollingMedian;
use features::{db_from_rms, rms, zero_crossing_rate, Spectrum};

pub const FRAME_SIZE: usize = 512;
pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    /// Below the noise gate, or not voice.
    Quiet,
    /// Voice, comfortably under the threshold.
    Calm { db: f32 },
    /// Voice within 3 dB below the threshold.
    GettingLoud { db: f32 },
    /// Voice over the threshold, and spectrally bright (both must agree).
    TooLoud { db: f32 },
}

/// Pure DSP cascade: energy gate -> VAD -> level vs baseline + tilt.
/// No allocation, no I/O, no locks. Calibration later replaces the
/// hardcoded noise floor / margin with per-device profiles.
pub struct Engine {
    spectrum: Spectrum,
    level_baseline: RollingMedian,
    tilt_baseline: RollingMedian,
    noise_floor_db: f32,
    margin_db: f32,
    unfed_streak: u32,
}

/// Voiced frames stuck above the feed cutoff (but not bright) for this many
/// frames in a row are a level shift, not a shout: retrain the baseline
/// instead of leaving the tray stuck yellow forever. ~5 s at 31 frames/sec.
const RETRAIN_AFTER_FRAMES: u32 = 150;

impl Engine {
    pub fn new() -> Self {
        Self {
            spectrum: Spectrum::new(FRAME_SIZE, SAMPLE_RATE as f32),
            // ~600 voiced frames = a few minutes of actual talking
            level_baseline: RollingMedian::new(600),
            tilt_baseline: RollingMedian::new(600),
            noise_floor_db: -55.0,
            margin_db: 7.0,
            unfed_streak: 0,
        }
    }

    pub fn process(&mut self, frame: &[f32]) -> State {
        let db = db_from_rms(rms(frame));
        if db < self.noise_floor_db + 3.0 {
            return State::Quiet;
        }
        let zcr = zero_crossing_rate(frame);
        let feat = self.spectrum.analyze(frame);
        if !vad::is_voiced(zcr, &feat) {
            return State::Quiet;
        }

        let threshold = self.level_baseline.median().unwrap_or(db) + self.margin_db;
        let tilt_ref = self.tilt_baseline.median().unwrap_or(feat.tilt_db);
        let bright = feat.tilt_db > tilt_ref + 3.0;
        // Only calm frames feed the baselines, so a long shout can't raise the threshold.
        if db < threshold - 3.0 {
            self.level_baseline.push(db);
            self.tilt_baseline.push(feat.tilt_db);
            self.unfed_streak = 0;
        } else if bright {
            // Genuine shouting: never let it drag the baseline up.
            self.unfed_streak = 0;
        } else {
            // Voiced, above the feed cutoff, but not bright: could be a mic
            // gain step rather than a raised voice. Let it retrain once it's
            // persisted long enough that a shout would be implausible.
            self.unfed_streak += 1;
            if self.unfed_streak > RETRAIN_AFTER_FRAMES {
                self.level_baseline.push(db);
                self.tilt_baseline.push(feat.tilt_db);
            }
        }

        if db >= threshold && bright {
            State::TooLoud { db }
        } else if db >= threshold - 3.0 {
            State::GettingLoud { db }
        } else {
            State::Calm { db }
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mix(rate: f32, n: usize, parts: &[(f32, f32)]) -> Vec<f32> {
        (0..n)
            .map(|i| {
                parts
                    .iter()
                    .map(|&(freq, amp)| (i as f32 / rate * freq * std::f32::consts::TAU).sin() * amp)
                    .sum()
            })
            .collect()
    }

    // Calm speech proxy: strong 200 Hz, faint 1.5 kHz -> ~-37 dB, dark tilt.
    fn quiet_voice() -> Vec<f32> {
        mix(16000.0, FRAME_SIZE, &[(200.0, 0.02), (1500.0, 0.004)])
    }

    // Raised speech proxy: much louder AND brighter (stronger high band).
    fn loud_voice() -> Vec<f32> {
        mix(16000.0, FRAME_SIZE, &[(200.0, 0.3), (2000.0, 0.15)])
    }

    #[test]
    fn silence_is_quiet() {
        let mut e = Engine::new();
        assert_eq!(e.process(&vec![0.0; FRAME_SIZE]), State::Quiet);
    }

    #[test]
    fn quiet_voice_is_calm_after_warmup() {
        let mut e = Engine::new();
        let frame = quiet_voice();
        let mut last = State::Quiet;
        for _ in 0..50 {
            last = e.process(&frame);
        }
        assert!(matches!(last, State::Calm { .. }), "got {last:?}");
    }

    #[test]
    fn loud_bright_voice_over_calm_baseline_is_too_loud() {
        let mut e = Engine::new();
        let quiet = quiet_voice();
        for _ in 0..50 {
            e.process(&quiet);
        }
        let state = e.process(&loud_voice());
        assert!(matches!(state, State::TooLoud { .. }), "got {state:?}");
    }

    #[test]
    fn shouting_does_not_drag_the_baseline_up() {
        let mut e = Engine::new();
        let quiet = quiet_voice();
        let loud = loud_voice();
        for _ in 0..50 {
            e.process(&quiet);
        }
        for _ in 0..200 {
            e.process(&loud);
        }
        // still flagged loud after 200 loud frames (~6.4 s)
        assert!(matches!(e.process(&loud), State::TooLoud { .. }));
    }

    #[test]
    fn below_noise_floor_is_quiet_even_if_periodic() {
        let mut e = Engine::new();
        let faint = mix(16000.0, FRAME_SIZE, &[(200.0, 0.0005)]); // ~-69 dB
        assert_eq!(e.process(&faint), State::Quiet);
    }

    #[test]
    fn baseline_recovers_after_gain_step() {
        let mut e = Engine::new();
        let quiet = quiet_voice();
        for _ in 0..50 {
            e.process(&quiet);
        }
        // mic gain jumps +12 dB: same voice, 4x amplitude, same spectral shape
        let boosted = mix(16000.0, FRAME_SIZE, &[(200.0, 0.08), (1500.0, 0.016)]);
        let mut recovered = false;
        for _ in 0..1000 {
            if matches!(e.process(&boosted), State::Calm { .. }) {
                recovered = true;
                break;
            }
        }
        assert!(recovered, "engine never re-adapted to the new gain level");
    }

    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ALLOC_COUNT: Cell<u64> = const { Cell::new(0) };
    }

    struct CountingAlloc;

    // SAFETY: delegates directly to System; the counter is thread-local
    // and touched outside the allocator's own allocation path.
    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOC_COUNT.with(|c| c.set(c.get() + 1));
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static A: CountingAlloc = CountingAlloc;

    #[test]
    fn process_never_allocates() {
        let mut e = Engine::new();
        let quiet = quiet_voice();
        let loud = loud_voice();
        // warm up outside the measured window
        for _ in 0..50 {
            e.process(&quiet);
        }
        let before = ALLOC_COUNT.with(|c| c.get());
        for _ in 0..200 {
            e.process(&quiet);
            e.process(&loud);
            e.process(&[0.0; FRAME_SIZE]);
        }
        let after = ALLOC_COUNT.with(|c| c.get());
        assert_eq!(after - before, 0, "hot path allocated {} times", after - before);
    }
}
