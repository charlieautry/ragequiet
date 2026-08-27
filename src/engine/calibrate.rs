use crate::engine::features::{db_from_rms, rms, zero_crossing_rate, Spectrum};
use crate::engine::{vad, FRAME_SIZE, SAMPLE_RATE};

/// Ceiling must sit meaningfully above the quiet point or the take is bad
/// (not loud enough, or input gain clipping).
pub const MIN_CEILING_GAP_DB: f32 = 3.0;

pub fn ceiling_is_sane(quiet_db: f32, ceiling_db: f32) -> bool {
    ceiling_db - quiet_db >= MIN_CEILING_GAP_DB
}

/// Collects per-frame levels and reports the median once enough frames arrived.
/// Pure: the wizard UI feeds it frames and polls progress. Not hot-path code.
pub struct Measurement {
    samples: Vec<f32>,
    scratch: Vec<f32>,
    target: usize,
    voiced_only: bool,
    spectrum: Spectrum,
    result: Option<f32>,
}

impl Measurement {
    /// Ambient level: every frame counts. ~3 s per the wizard script.
    pub fn noise_floor() -> Self {
        Self::new(3.0, false)
    }

    /// Speaking level: only voiced frames count.
    pub fn voiced_level(seconds: f32) -> Self {
        Self::new(seconds, true)
    }

    fn new(seconds: f32, voiced_only: bool) -> Self {
        let frames_per_sec = SAMPLE_RATE as usize / FRAME_SIZE; // 31
        let target = ((seconds * frames_per_sec as f32) as usize).max(1);
        Self {
            samples: Vec::with_capacity(target),
            scratch: Vec::with_capacity(target),
            target,
            voiced_only,
            spectrum: Spectrum::new(FRAME_SIZE, SAMPLE_RATE as f32),
            result: None,
        }
    }

    /// Feed one frame; Some(median dB) once the target frame count is reached.
    /// Allocation-free once complete: a wizard step can keep feeding a
    /// finished `Measurement` (e.g. while waiting on the UI) without cost.
    pub fn push(&mut self, frame: &[f32]) -> Option<f32> {
        if let Some(r) = self.result {
            return Some(r);
        }
        if self.samples.len() >= self.target {
            return Some(self.complete());
        }
        let db = db_from_rms(rms(frame));
        if self.voiced_only {
            let feat = self.spectrum.analyze(frame);
            if !vad::is_voiced(zero_crossing_rate(frame), &feat) {
                return None;
            }
        }
        self.samples.push(db);
        if self.samples.len() >= self.target {
            Some(self.complete())
        } else {
            None
        }
    }

    pub fn progress(&self) -> f32 {
        self.samples.len() as f32 / self.target as f32
    }

    fn complete(&mut self) -> f32 {
        let n = self.samples.len();
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.samples);
        let mid = n / 2;
        self.scratch.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
        let median = self.scratch[mid];
        self.result = Some(median);
        median
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::FRAME_SIZE;

    fn sine_frame(freq: f32, amp: f32) -> Vec<f32> {
        (0..FRAME_SIZE)
            .map(|i| (i as f32 / 16000.0 * freq * std::f32::consts::TAU).sin() * amp)
            .collect()
    }

    #[test]
    fn noise_floor_measures_ambient_over_all_frames() {
        let mut m = Measurement::noise_floor();
        let ambient = sine_frame(200.0, 0.001); // ~-63 dB
        let mut result = None;
        for _ in 0..200 {
            result = m.push(&ambient);
            if result.is_some() { break; }
        }
        let db = result.expect("did not complete within 200 frames");
        assert!((-66.0..=-60.0).contains(&db), "got {db}");
    }

    #[test]
    fn voiced_level_ignores_silence() {
        let mut m = Measurement::voiced_level(1.0); // ~31 frames of voice needed
        let silence = vec![0.0f32; FRAME_SIZE];
        for _ in 0..100 {
            assert!(m.push(&silence).is_none());
        }
        assert_eq!(m.progress(), 0.0, "silence must not advance progress");
        let voice = sine_frame(200.0, 0.02); // ~-37 dB, passes VAD
        let mut result = None;
        for _ in 0..100 {
            result = m.push(&voice);
            if result.is_some() { break; }
        }
        let db = result.expect("voiced frames did not complete the measurement");
        assert!((-40.0..=-34.0).contains(&db), "got {db}");
    }

    #[test]
    fn progress_advances_toward_one() {
        let mut m = Measurement::voiced_level(1.0);
        let voice = sine_frame(200.0, 0.02);
        assert_eq!(m.progress(), 0.0);
        m.push(&voice);
        assert!(m.progress() > 0.0 && m.progress() < 1.0);
    }

    #[test]
    fn ceiling_sanity_check() {
        assert!(ceiling_is_sane(-34.0, -18.0));
        assert!(!ceiling_is_sane(-34.0, -32.5)); // within 3 dB: something's off
        assert!(!ceiling_is_sane(-34.0, -35.0)); // quieter than quiet point
    }
}
