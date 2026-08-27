use crate::alert::AlertGate;
use crate::bridge::MeasurementKind;
use crate::engine::calibrate::Measurement;
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
    /// Some(..) only on a progress-percent change or on completion of an
    /// active calibration measurement; app.rs forwards it to the wizard.
    pub measurement: Option<MeasurementUpdate>,
}

/// Progress/completion events for an in-flight calibration measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MeasurementUpdate {
    Progress(f32),
    Complete(MeasurementKind, f32),
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
    /// Active calibration measurement, if any: kind, the pure accumulator,
    /// and the last whole-percent progress value emitted for it (so repeat
    /// frames at the same percent don't spam the UI).
    measurement: Option<(MeasurementKind, Measurement, u32)>,
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
            measurement: None,
        }
    }

    pub fn on_frame(&mut self, frame: &[f32], now_ms: u64) -> FrameOutcome {
        // The measurement, if any, sees every frame first: it must not miss
        // a sample to detection logic running underneath it.
        let measuring = self.measurement.is_some();
        let measurement = if let Some((kind, m, last_pct)) = self.measurement.as_mut() {
            let kind = *kind;
            if let Some(db) = m.push(frame) {
                self.measurement = None;
                Some(MeasurementUpdate::Complete(kind, db))
            } else {
                let p = m.progress();
                let pct = (p * 100.0) as u32;
                if pct != *last_pct {
                    *last_pct = pct;
                    Some(MeasurementUpdate::Progress(p))
                } else {
                    None
                }
            }
        } else {
            None
        };

        let state = self.engine.process(frame);
        // Test mode still drives the icon and the meter; it only mutes the
        // speaker, so the gate keeps running and its timing stays honest.
        // Measurements suppress the beep the same way (the ceiling step asks
        // the user to be loud on purpose) while still updating the gate so
        // its cooldown state stays sane once the measurement ends.
        let beep = self
            .gate
            .update(matches!(state, State::TooLoud { .. }), now_ms)
            && !self.test_mode
            && !measuring;
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
        FrameOutcome { state_change, beep, measurement }
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

    /// Baseline drift since calibration (dB); NaN when uncalibrated. Cheap:
    /// it reads a value the engine already cached while computing this
    /// frame's threshold, rather than running a second median.
    pub fn drift_db(&self) -> f32 {
        self.engine.baseline_drift_db()
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
            Command::StartMeasurement(kind, m) => {
                self.measurement = Some((kind, *m, 0));
            }
            Command::CancelMeasurement => self.measurement = None,
        }
    }
}

/// Kind -> `Measurement` constructor mapping. Built on the UI thread (it
/// allocates: sample buffers + a realfft planner) and handed through
/// `Command::StartMeasurement` pre-built, so the audio callback's command
/// drain in `apply` never allocates.
pub fn measurement_for(kind: MeasurementKind) -> Measurement {
    match kind {
        MeasurementKind::NoiseFloor => Measurement::noise_floor(),
        MeasurementKind::QuietPoint => Measurement::voiced_level(8.0),
        MeasurementKind::Ceiling => Measurement::voiced_level(5.0),
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

    fn silent_frame_at(now: u64, d: &mut Detector) -> FrameOutcome {
        d.on_frame(&silence(), now)
    }

    #[test]
    fn noise_floor_measurement_completes_on_silence() {
        let mut d = test_detector();
        d.apply(crate::bridge::Command::StartMeasurement(
            crate::bridge::MeasurementKind::NoiseFloor,
            Box::new(measurement_for(crate::bridge::MeasurementKind::NoiseFloor)),
        ));
        let mut completed = None;
        for i in 0..200u64 {
            let out = silent_frame_at(i * FRAME_MS, &mut d);
            if let Some(MeasurementUpdate::Complete(kind, db)) = out.measurement {
                completed = Some((kind, db));
                break;
            }
        }
        let (kind, db) = completed.expect("noise floor measurement did not complete");
        assert_eq!(kind, crate::bridge::MeasurementKind::NoiseFloor);
        assert!(db <= -60.0, "expected a plausible silent noise floor, got {db}");

        // measurement is cleared after completion: the very next frame emits nothing
        let out = silent_frame_at(200 * FRAME_MS, &mut d);
        assert!(out.measurement.is_none(), "measurement must clear after completion");
    }

    #[test]
    fn quiet_point_measurement_ignores_silence_then_completes_on_voice() {
        let mut d = test_detector();
        d.apply(crate::bridge::Command::StartMeasurement(
            crate::bridge::MeasurementKind::QuietPoint,
            Box::new(measurement_for(crate::bridge::MeasurementKind::QuietPoint)),
        ));
        let mut emitted_progress = 0u32;
        for i in 0..50u64 {
            let out = silent_frame_at(i * FRAME_MS, &mut d);
            match out.measurement {
                Some(MeasurementUpdate::Progress(p)) => {
                    assert_eq!(p, 0.0, "silence must not advance quiet-point progress");
                    emitted_progress += 1;
                }
                Some(MeasurementUpdate::Complete(..)) => panic!("silence must not complete a voiced measurement"),
                None => {}
            }
        }
        assert_eq!(emitted_progress, 0, "silence must not emit any progress events");

        let voice = quiet_voice();
        let mut completed = None;
        for i in 0..400u64 {
            let now = 50 * FRAME_MS + i * FRAME_MS;
            let out = d.on_frame(&voice, now);
            if let Some(MeasurementUpdate::Complete(kind, db)) = out.measurement {
                completed = Some((kind, db));
                break;
            }
        }
        let (kind, db) = completed.expect("quiet point measurement did not complete on voiced frames");
        assert_eq!(kind, crate::bridge::MeasurementKind::QuietPoint);
        assert!((-40.0..=-34.0).contains(&db), "got {db}");
    }

    #[test]
    fn progress_events_emit_only_on_whole_percent_change() {
        let mut d = test_detector();
        d.apply(crate::bridge::Command::StartMeasurement(
            crate::bridge::MeasurementKind::QuietPoint,
            Box::new(measurement_for(crate::bridge::MeasurementKind::QuietPoint)),
        ));
        let voice = quiet_voice();
        let mut emitted_pcts = Vec::new();
        let mut completed = false;
        for i in 0..400u64 {
            let now = i * FRAME_MS;
            let out = d.on_frame(&voice, now);
            match out.measurement {
                Some(MeasurementUpdate::Progress(p)) => emitted_pcts.push((p * 100.0) as u32),
                Some(MeasurementUpdate::Complete(..)) => {
                    completed = true;
                    break;
                }
                None => {}
            }
        }
        assert!(completed, "measurement should complete within 400 voiced frames");
        assert!(!emitted_pcts.is_empty(), "expected at least one progress event");
        // no spam: every emitted percent must be strictly greater than the last
        let mut last = None;
        for &p in &emitted_pcts {
            if let Some(prev) = last {
                assert!(p > prev, "progress percent must strictly increase, got {emitted_pcts:?}");
            }
            last = Some(p);
        }
        let distinct_crossed = emitted_pcts.last().copied().unwrap_or(0) - emitted_pcts.first().copied().unwrap_or(0) + 1;
        assert!(
            emitted_pcts.len() as u32 <= distinct_crossed,
            "emitted {} progress events but only {} distinct percents were crossed",
            emitted_pcts.len(),
            distinct_crossed
        );
    }

    #[test]
    fn cancel_measurement_stops_updates() {
        let mut d = test_detector();
        d.apply(crate::bridge::Command::StartMeasurement(
            crate::bridge::MeasurementKind::QuietPoint,
            Box::new(measurement_for(crate::bridge::MeasurementKind::QuietPoint)),
        ));
        let voice = quiet_voice();
        // get a bit of progress going first
        for i in 0..5u64 {
            d.on_frame(&voice, i * FRAME_MS);
        }
        d.apply(crate::bridge::Command::CancelMeasurement);
        for i in 5..50u64 {
            let out = d.on_frame(&voice, i * FRAME_MS);
            assert!(out.measurement.is_none(), "frame {i} emitted a measurement update after cancel");
        }
    }

    #[test]
    fn beep_suppressed_during_measurement_then_functional_after_cancel() {
        let mut d = test_detector();
        let start = warm_up(&mut d);
        d.apply(crate::bridge::Command::StartMeasurement(
            crate::bridge::MeasurementKind::Ceiling,
            Box::new(measurement_for(crate::bridge::MeasurementKind::Ceiling)),
        ));
        let loud = loud_voice();
        for i in 0..40u64 {
            let out = d.on_frame(&loud, start + i * FRAME_MS);
            assert!(!out.beep, "beep must be suppressed during an active measurement (frame {i})");
        }
        d.apply(crate::bridge::Command::CancelMeasurement);
        let resume_start = start + 40 * FRAME_MS;
        let fired = first_beep_offset_ms(&mut d, resume_start, 80);
        assert!(fired.is_some(), "beep must be functional again after the measurement ends");
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
