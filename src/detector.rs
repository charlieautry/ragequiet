use crate::alert::AlertGate;
use crate::engine::{Engine, State};

/// Which brand tray icon should be showing; the actual colors/pixels live in
/// the brand PNGs (`assets/brand/tray-*-32.png`), decoded once at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Quiet,
    Warning,
    Loud,
}

pub fn tray_state_for(state: State) -> TrayState {
    match state {
        State::Quiet | State::Calm { .. } => TrayState::Quiet,
        State::GettingLoud { .. } => TrayState::Warning,
        State::TooLoud { .. } => TrayState::Loud,
    }
}

/// Per-frame outcome for the UI thread.
pub struct FrameOutcome {
    /// Some(state) only when the tray icon must change.
    pub state_change: Option<TrayState>,
    pub beep: bool,
}

/// Level reported for frames the cascade rejected (silence / non-voice).
pub const SILENT_DB: f32 = -100.0;

/// How long a ghost peak lingers before it decays back to the live level.
const PEAK_HOLD_MS: u64 = 3000;

fn level_of(state: State) -> f32 {
    match state {
        State::Quiet => SILENT_DB,
        State::Calm { db } | State::GettingLoud { db } | State::TooLoud { db } => db,
    }
}

/// Owns all per-frame state so the audio callback stays a thin shim
/// and the logic is testable without an audio device.
pub struct Detector {
    engine: Engine,
    gate: AlertGate,
    last_state: Option<TrayState>,
    test_mode: bool,
    last_level_db: f32,
    peak_db: f32,
    peak_at_ms: u64,
}

impl Detector {
    pub fn new(engine: Engine, gate: AlertGate) -> Self {
        Self {
            engine,
            gate,
            last_state: Some(TrayState::Quiet),
            test_mode: false,
            last_level_db: SILENT_DB,
            peak_db: SILENT_DB,
            peak_at_ms: 0,
        }
    }

    pub fn on_frame(&mut self, frame: &[f32], now_ms: u64) -> FrameOutcome {
        let state = self.engine.process(frame);
        // Test mode still drives the icon and the meter; it only mutes the
        // speaker, so the gate keeps running and its timing stays honest.
        let beep = self
            .gate
            .update(matches!(state, State::TooLoud { .. }), now_ms)
            && !self.test_mode;
        let level_db = level_of(state);
        self.last_level_db = level_db;
        // `>=`, not `>`: a frame that merely ties the peak is still a fresh
        // sighting of it, and must restart the 3 s window rather than let a
        // steady level age out from under itself.
        if level_db >= self.peak_db || now_ms.saturating_sub(self.peak_at_ms) > PEAK_HOLD_MS {
            self.peak_db = level_db;
            self.peak_at_ms = now_ms;
        }
        let tray_state = tray_state_for(state);
        let state_change = if self.last_state != Some(tray_state) {
            self.last_state = Some(tray_state);
            Some(tray_state)
        } else {
            None
        };
        FrameOutcome { state_change, beep }
    }

    /// Called when monitoring resumes after a pause: the next frame must
    /// re-announce its state, and a shout in progress must re-earn the hold.
    pub fn resume(&mut self) {
        self.last_state = None;
        self.gate.reset();
    }

    /// dB of the most recent frame; `SILENT_DB` when the cascade said Quiet.
    pub fn last_level_db(&self) -> f32 {
        self.last_level_db
    }

    /// Live threshold including adaptive drift (`&mut`: the median is lazy).
    pub fn threshold_db(&mut self) -> f32 {
        self.engine.threshold_db()
    }

    /// Highest level seen in the last `PEAK_HOLD_MS`.
    pub fn peak_db(&self) -> f32 {
        self.peak_db
    }

    /// Apply a UI command in the audio callback, between frames.
    pub fn apply(&mut self, cmd: crate::bridge::Command) {
        use crate::bridge::Command;
        match cmd {
            Command::SetTuning(tuning) => self.engine.set_tuning(tuning),
            Command::SetGate {
                hold_ms,
                cooldown_ms,
            } => self.gate = AlertGate::new(hold_ms, cooldown_ms),
            Command::SetTestMode(on) => self.test_mode = on,
            Command::SetEnabledIconBaseline => self.resume(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::FRAME_SIZE;

    fn silence() -> Vec<f32> {
        vec![0.0; FRAME_SIZE]
    }

    fn test_detector() -> Detector {
        Detector::new(Engine::new(), AlertGate::new(300, 3000))
    }

    #[test]
    fn first_frame_reports_no_change_when_already_quiet() {
        let mut d = test_detector();
        let out = d.on_frame(&silence(), 0);
        assert!(out.state_change.is_none());
        assert!(!out.beep);
    }

    #[test]
    fn resume_forces_state_reannouncement() {
        let mut d = test_detector();
        assert!(d.on_frame(&silence(), 0).state_change.is_none());
        d.resume();
        // same state as before, but after resume it must be re-sent
        assert_eq!(d.on_frame(&silence(), 32).state_change, Some(TrayState::Quiet));
    }

    #[test]
    fn steady_state_sends_nothing() {
        let mut d = test_detector();
        for i in 0..100 {
            let out = d.on_frame(&silence(), i * 32);
            assert!(out.state_change.is_none(), "frame {i} sent a change");
        }
    }

    // Local copies of the engine tests' signal generators: the detector tests
    // need a real cascade to reach TooLoud, and the engine's own helpers live
    // in its private test module.
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

    fn quiet_voice() -> Vec<f32> {
        mix(16000.0, FRAME_SIZE, &[(200.0, 0.02), (1500.0, 0.004)])
    }

    fn loud_voice() -> Vec<f32> {
        mix(16000.0, FRAME_SIZE, &[(200.0, 0.3), (2000.0, 0.15)])
    }

    const FRAME_MS: u64 = 32;
    const WARMUP_FRAMES: u64 = 50;

    /// Feed calm speech until the engine's baselines are trained.
    fn warm_up(d: &mut Detector) -> u64 {
        let quiet = quiet_voice();
        for i in 0..WARMUP_FRAMES {
            d.on_frame(&quiet, i * FRAME_MS);
        }
        WARMUP_FRAMES * FRAME_MS
    }

    /// ms (relative to the first loud frame) at which the gate fired, if it did.
    fn first_beep_offset_ms(d: &mut Detector, start_ms: u64, frames: u64) -> Option<u64> {
        let loud = loud_voice();
        for i in 0..frames {
            let now = start_ms + i * FRAME_MS;
            if d.on_frame(&loud, now).beep {
                return Some(now - start_ms);
            }
        }
        None
    }

    #[test]
    fn test_mode_suppresses_the_beep_but_not_the_color() {
        let mut loud_marker = None;
        let mut d = test_detector();
        d.apply(crate::bridge::Command::SetTestMode(true));
        let start = warm_up(&mut d);
        let loud = loud_voice();
        for i in 0..40 {
            let out = d.on_frame(&loud, start + i * FRAME_MS);
            assert!(!out.beep, "test mode must never beep (frame {i})");
            if let Some(s) = out.state_change {
                loud_marker = Some(s);
            }
        }
        assert_eq!(
            loud_marker,
            Some(TrayState::Loud),
            "the icon must still go to the loud state in test mode"
        );

        // control: the same drive without test mode does beep
        let mut d = test_detector();
        let start = warm_up(&mut d);
        assert!(
            first_beep_offset_ms(&mut d, start, 40).is_some(),
            "the control run must beep, otherwise the suppression test proves nothing"
        );
    }

    #[test]
    fn peak_decays_after_three_seconds_of_quiet() {
        let mut d = test_detector();
        let end = warm_up(&mut d);
        let voiced_peak = d.peak_db();
        assert!(
            voiced_peak > -60.0 && voiced_peak.is_finite(),
            "voiced frames must set a peak, got {voiced_peak}"
        );

        // 2 s of silence: the ghost peak is still holding
        let mut now = end;
        for _ in 0..(2000 / FRAME_MS) {
            d.on_frame(&silence(), now);
            now += FRAME_MS;
        }
        assert_eq!(d.peak_db(), voiced_peak, "peak must hold for ~3 s");

        // past 3 s it decays to the live (silent) level
        for _ in 0..(2000 / FRAME_MS) {
            d.on_frame(&silence(), now);
            now += FRAME_MS;
        }
        assert_eq!(d.peak_db(), SILENT_DB, "stale peak must decay");
    }

    #[test]
    fn apply_set_gate_changes_beep_timing() {
        let mut fast = test_detector();
        let start = warm_up(&mut fast);
        let fast_at = first_beep_offset_ms(&mut fast, start, 80).expect("300 ms hold must fire");
        assert!(fast_at < 500, "300 ms hold fired at {fast_at} ms");

        let mut slow = test_detector();
        let start = warm_up(&mut slow);
        slow.apply(crate::bridge::Command::SetGate { hold_ms: 1500, cooldown_ms: 3000 });
        let slow_at = first_beep_offset_ms(&mut slow, start, 80).expect("1500 ms hold must fire");
        assert!(slow_at >= 1500, "1500 ms hold fired at {slow_at} ms");
    }

    #[test]
    fn apply_enabled_icon_baseline_reannounces_state() {
        let mut d = test_detector();
        assert!(d.on_frame(&silence(), 0).state_change.is_none());
        d.apply(crate::bridge::Command::SetEnabledIconBaseline);
        assert_eq!(d.on_frame(&silence(), 32).state_change, Some(TrayState::Quiet));
    }

    #[test]
    fn apply_set_tuning_moves_the_threshold() {
        let mut d = test_detector();
        d.apply(crate::bridge::Command::SetTuning(crate::engine::Tuning {
            noise_floor_db: -58.0,
            quiet_db: Some(-37.0),
            ceiling_db: Some(-17.0),
            sensitivity: 0.0,
        }));
        let low = d.threshold_db();
        d.apply(crate::bridge::Command::SetTuning(crate::engine::Tuning {
            noise_floor_db: -58.0,
            quiet_db: Some(-37.0),
            ceiling_db: Some(-17.0),
            sensitivity: 1.0,
        }));
        assert!(d.threshold_db() > low + 15.0, "sensitivity must move the threshold");
    }
}
