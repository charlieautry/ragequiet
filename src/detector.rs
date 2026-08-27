use crate::alert::AlertGate;
use crate::engine::{Engine, State};

pub const GREEN: [u8; 3] = [46, 204, 113];
pub const YELLOW: [u8; 3] = [241, 196, 15];
pub const RED: [u8; 3] = [231, 76, 60];
pub const GREY: [u8; 3] = [127, 140, 141];

pub fn color_for(state: State) -> [u8; 3] {
    match state {
        State::Quiet | State::Calm { .. } => GREEN,
        State::GettingLoud { .. } => YELLOW,
        State::TooLoud { .. } => RED,
    }
}

/// Per-frame outcome for the UI thread.
pub struct FrameOutcome {
    /// Some(color) only when the tray icon must change.
    pub color_change: Option<[u8; 3]>,
    pub beep: bool,
}

/// Owns all per-frame state so the audio callback stays a thin shim
/// and the logic is testable without an audio device.
pub struct Detector {
    engine: Engine,
    gate: AlertGate,
    last_color: Option<[u8; 3]>,
}

impl Detector {
    pub fn new(engine: Engine, gate: AlertGate) -> Self {
        Self {
            engine,
            gate,
            last_color: Some(GREEN),
        }
    }

    pub fn on_frame(&mut self, frame: &[f32], now_ms: u64) -> FrameOutcome {
        let state = self.engine.process(frame);
        let beep = self
            .gate
            .update(matches!(state, State::TooLoud { .. }), now_ms);
        let color = color_for(state);
        let color_change = if self.last_color != Some(color) {
            self.last_color = Some(color);
            Some(color)
        } else {
            None
        };
        FrameOutcome { color_change, beep }
    }

    /// Called when monitoring resumes after a pause: the next frame must
    /// re-announce its color, and a shout in progress must re-earn the hold.
    pub fn resume(&mut self) {
        self.last_color = None;
        self.gate.reset();
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
    fn first_frame_reports_no_change_when_already_green() {
        let mut d = test_detector();
        let out = d.on_frame(&silence(), 0);
        assert!(out.color_change.is_none());
        assert!(!out.beep);
    }

    #[test]
    fn resume_forces_color_reannouncement() {
        let mut d = test_detector();
        assert!(d.on_frame(&silence(), 0).color_change.is_none());
        d.resume();
        // same state as before, but after resume the color must be re-sent
        assert_eq!(d.on_frame(&silence(), 32).color_change, Some(GREEN));
    }

    #[test]
    fn steady_state_sends_nothing() {
        let mut d = test_detector();
        for i in 0..100 {
            let out = d.on_frame(&silence(), i * 32);
            assert!(out.color_change.is_none(), "frame {i} sent a change");
        }
    }
}
