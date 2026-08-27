//! Pure calibration-wizard state machine (Phase 2c Task 2). No iced imports
//! here — the view half is added in Task 3. app.rs drives this by feeding
//! `WizardEvent`s translated from `Command::StartMeasurement` completions and
//! button presses, and starts/cancels measurements per the `Option<MeasurementKind>`
//! this returns.

use crate::bridge::MeasurementKind;
use crate::engine::calibrate::ceiling_is_sane;

#[derive(Debug, Clone, PartialEq)]
pub enum WizardStep {
    Intro,
    Measuring {
        kind: MeasurementKind,
        progress: f32,
    },
    NoiseDone {
        noise_db: f32,
    },
    QuietDone {
        noise_db: f32,
        quiet_db: f32,
        skip_ceiling_default: bool,
    },
    CeilingRetry {
        noise_db: f32,
        quiet_db: f32,
        gap_db: f32,
    },
    Done {
        result: WizardResult,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WizardResult {
    pub noise_floor_db: f32,
    pub quiet_db: f32,
    pub ceiling_db: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WizardEvent {
    /// Intro -> start noise measurement
    Begin,
    Progress(f32),
    Complete(MeasurementKind, f32),
    /// NoiseDone -> start quiet; QuietDone -> start ceiling; CeilingRetry -> retry ceiling
    Continue,
    /// QuietDone -> Done without ceiling
    SkipCeiling,
}

pub struct Wizard {
    pub step: WizardStep,
    /// Local hour 0-23 at wizard start; late night (22:00-04:59) defaults the
    /// ceiling step to skipped (people start this app at night next to
    /// sleepers).
    start_hour: u32,
    /// Carried forward across steps since `WizardStep::Measuring` itself
    /// only tracks the kind in progress, not prior results.
    noise_db: Option<f32>,
    quiet_db: Option<f32>,
}

impl Wizard {
    pub fn new(start_hour: u32) -> Self {
        Self {
            step: WizardStep::Intro,
            start_hour,
            noise_db: None,
            quiet_db: None,
        }
    }

    fn skip_ceiling_default(&self) -> bool {
        self.start_hour >= 22 || self.start_hour < 5
    }

    /// Returns a measurement to start, if the transition requires one.
    pub fn on_event(&mut self, event: WizardEvent) -> Option<MeasurementKind> {
        use MeasurementKind::{Ceiling, NoiseFloor, QuietPoint};
        use WizardEvent::{Begin, Complete, Continue, Progress, SkipCeiling};
        use WizardStep::{CeilingRetry, Done, Measuring, NoiseDone, QuietDone};

        let (new_step, started) = match (self.step.clone(), event) {
            (WizardStep::Intro, Begin) => (
                Measuring { kind: NoiseFloor, progress: 0.0 },
                Some(NoiseFloor),
            ),

            (Measuring { kind, .. }, Progress(p)) => {
                (Measuring { kind, progress: p }, None)
            }

            (Measuring { kind: NoiseFloor, .. }, Complete(NoiseFloor, db)) => {
                self.noise_db = Some(db);
                (NoiseDone { noise_db: db }, None)
            }

            (NoiseDone { noise_db }, Continue) => {
                self.noise_db = Some(noise_db);
                (
                    Measuring { kind: QuietPoint, progress: 0.0 },
                    Some(QuietPoint),
                )
            }

            (Measuring { kind: QuietPoint, .. }, Complete(QuietPoint, db)) => {
                let noise_db = self.noise_db.unwrap_or(f32::NAN);
                self.quiet_db = Some(db);
                (
                    QuietDone {
                        noise_db,
                        quiet_db: db,
                        skip_ceiling_default: self.skip_ceiling_default(),
                    },
                    None,
                )
            }

            (QuietDone { noise_db, quiet_db, .. }, Continue) => {
                self.noise_db = Some(noise_db);
                self.quiet_db = Some(quiet_db);
                (Measuring { kind: Ceiling, progress: 0.0 }, Some(Ceiling))
            }

            (QuietDone { noise_db, quiet_db, .. }, SkipCeiling) => (
                Done {
                    result: WizardResult {
                        noise_floor_db: noise_db,
                        quiet_db,
                        ceiling_db: None,
                    },
                },
                None,
            ),

            (Measuring { kind: Ceiling, .. }, Complete(Ceiling, db)) => {
                let noise_db = self.noise_db.unwrap_or(f32::NAN);
                let quiet_db = self.quiet_db.unwrap_or(f32::NAN);
                if ceiling_is_sane(quiet_db, db) {
                    (
                        Done {
                            result: WizardResult {
                                noise_floor_db: noise_db,
                                quiet_db,
                                ceiling_db: Some(db),
                            },
                        },
                        None,
                    )
                } else {
                    (
                        CeilingRetry { noise_db, quiet_db, gap_db: db - quiet_db },
                        None,
                    )
                }
            }

            (CeilingRetry { noise_db, quiet_db, .. }, Continue) => {
                self.noise_db = Some(noise_db);
                self.quiet_db = Some(quiet_db);
                (Measuring { kind: Ceiling, progress: 0.0 }, Some(Ceiling))
            }

            (CeilingRetry { noise_db, quiet_db, .. }, SkipCeiling) => (
                Done {
                    result: WizardResult {
                        noise_floor_db: noise_db,
                        quiet_db,
                        ceiling_db: None,
                    },
                },
                None,
            ),

            // Stale/mismatched events: wrong-kind Complete during Measuring,
            // Progress/Complete outside Measuring, Begin outside Intro,
            // Continue/SkipCeiling in steps where they are not defined.
            // Ignored: no state change, no measurement started.
            (unchanged, _) => (unchanged, None),
        };
        self.step = new_step;
        started
    }

    /// What to cancel if the wizard is abandoned while a measurement runs.
    /// `Some(kind)` iff currently in a `Measuring` step.
    pub fn cancelled_measurement(&self) -> Option<MeasurementKind> {
        match &self.step {
            WizardStep::Measuring { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::MeasurementKind::{Ceiling, NoiseFloor, QuietPoint};

    #[test]
    fn happy_path_with_ceiling_threads_values_through() {
        let mut w = Wizard::new(14); // mid-afternoon, no late-night default
        assert_eq!(w.step, WizardStep::Intro);
        assert_eq!(w.cancelled_measurement(), None);

        let started = w.on_event(WizardEvent::Begin);
        assert_eq!(started, Some(NoiseFloor));
        assert_eq!(
            w.step,
            WizardStep::Measuring { kind: NoiseFloor, progress: 0.0 }
        );
        assert_eq!(w.cancelled_measurement(), Some(NoiseFloor));

        let started = w.on_event(WizardEvent::Progress(0.5));
        assert_eq!(started, None);
        assert_eq!(
            w.step,
            WizardStep::Measuring { kind: NoiseFloor, progress: 0.5 }
        );

        let started = w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        assert_eq!(started, None);
        assert_eq!(w.step, WizardStep::NoiseDone { noise_db: -60.0 });
        assert_eq!(w.cancelled_measurement(), None);

        let started = w.on_event(WizardEvent::Continue);
        assert_eq!(started, Some(QuietPoint));
        assert_eq!(
            w.step,
            WizardStep::Measuring { kind: QuietPoint, progress: 0.0 }
        );

        let started = w.on_event(WizardEvent::Complete(QuietPoint, -34.0));
        assert_eq!(started, None);
        assert_eq!(
            w.step,
            WizardStep::QuietDone {
                noise_db: -60.0,
                quiet_db: -34.0,
                skip_ceiling_default: false,
            }
        );

        let started = w.on_event(WizardEvent::Continue);
        assert_eq!(started, Some(Ceiling));
        assert_eq!(
            w.step,
            WizardStep::Measuring { kind: Ceiling, progress: 0.0 }
        );

        let started = w.on_event(WizardEvent::Complete(Ceiling, -18.0));
        assert_eq!(started, None);
        match &w.step {
            WizardStep::Done { result } => {
                assert_eq!(
                    *result,
                    WizardResult {
                        noise_floor_db: -60.0,
                        quiet_db: -34.0,
                        ceiling_db: Some(-18.0),
                    }
                );
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(w.cancelled_measurement(), None);
    }

    #[test]
    fn skip_path_from_quiet_done_reaches_done_without_ceiling() {
        let mut w = Wizard::new(14);
        w.on_event(WizardEvent::Begin);
        w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        w.on_event(WizardEvent::Continue);
        w.on_event(WizardEvent::Complete(QuietPoint, -34.0));

        let started = w.on_event(WizardEvent::SkipCeiling);
        assert_eq!(started, None);
        assert_eq!(
            w.step,
            WizardStep::Done {
                result: WizardResult {
                    noise_floor_db: -60.0,
                    quiet_db: -34.0,
                    ceiling_db: None,
                },
            }
        );
    }

    #[test]
    fn late_night_flag_hour_21_is_false() {
        let mut w = Wizard::new(21);
        w.on_event(WizardEvent::Begin);
        w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        w.on_event(WizardEvent::Continue);
        w.on_event(WizardEvent::Complete(QuietPoint, -34.0));
        match &w.step {
            WizardStep::QuietDone { skip_ceiling_default, .. } => {
                assert!(!skip_ceiling_default);
            }
            other => panic!("expected QuietDone, got {other:?}"),
        }
    }

    #[test]
    fn late_night_flag_hour_22_is_true() {
        let mut w = Wizard::new(22);
        w.on_event(WizardEvent::Begin);
        w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        w.on_event(WizardEvent::Continue);
        w.on_event(WizardEvent::Complete(QuietPoint, -34.0));
        match &w.step {
            WizardStep::QuietDone { skip_ceiling_default, .. } => {
                assert!(*skip_ceiling_default);
            }
            other => panic!("expected QuietDone, got {other:?}"),
        }
    }

    #[test]
    fn late_night_flag_hour_4_is_true() {
        let mut w = Wizard::new(4);
        w.on_event(WizardEvent::Begin);
        w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        w.on_event(WizardEvent::Continue);
        w.on_event(WizardEvent::Complete(QuietPoint, -34.0));
        match &w.step {
            WizardStep::QuietDone { skip_ceiling_default, .. } => {
                assert!(*skip_ceiling_default);
            }
            other => panic!("expected QuietDone, got {other:?}"),
        }
    }

    #[test]
    fn late_night_flag_hour_5_is_false() {
        let mut w = Wizard::new(5);
        w.on_event(WizardEvent::Begin);
        w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        w.on_event(WizardEvent::Continue);
        w.on_event(WizardEvent::Complete(QuietPoint, -34.0));
        match &w.step {
            WizardStep::QuietDone { skip_ceiling_default, .. } => {
                assert!(!skip_ceiling_default);
            }
            other => panic!("expected QuietDone, got {other:?}"),
        }
    }

    fn to_quiet_done(start_hour: u32) -> Wizard {
        let mut w = Wizard::new(start_hour);
        w.on_event(WizardEvent::Begin);
        w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        w.on_event(WizardEvent::Continue);
        w.on_event(WizardEvent::Complete(QuietPoint, -34.0));
        w
    }

    #[test]
    fn insane_ceiling_retries_then_succeeds() {
        let mut w = to_quiet_done(14);
        w.on_event(WizardEvent::Continue); // -> Measuring(Ceiling)

        // Within MIN_CEILING_GAP_DB (3.0) of quiet_db (-34.0): insane.
        let started = w.on_event(WizardEvent::Complete(Ceiling, -32.5));
        assert_eq!(started, None);
        match &w.step {
            WizardStep::CeilingRetry { noise_db, quiet_db, gap_db } => {
                assert_eq!(*noise_db, -60.0);
                assert_eq!(*quiet_db, -34.0);
                assert!((*gap_db - 1.5).abs() < 1e-6, "got {gap_db}");
            }
            other => panic!("expected CeilingRetry, got {other:?}"),
        }
        assert_eq!(w.cancelled_measurement(), None);

        let started = w.on_event(WizardEvent::Continue);
        assert_eq!(started, Some(Ceiling));
        assert_eq!(
            w.step,
            WizardStep::Measuring { kind: Ceiling, progress: 0.0 }
        );

        let started = w.on_event(WizardEvent::Complete(Ceiling, -18.0));
        assert_eq!(started, None);
        match &w.step {
            WizardStep::Done { result } => {
                assert_eq!(
                    *result,
                    WizardResult {
                        noise_floor_db: -60.0,
                        quiet_db: -34.0,
                        ceiling_db: Some(-18.0),
                    }
                );
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn insane_ceiling_retry_then_skip() {
        let mut w = to_quiet_done(14);
        w.on_event(WizardEvent::Continue);
        w.on_event(WizardEvent::Complete(Ceiling, -35.0)); // quieter than quiet point

        let started = w.on_event(WizardEvent::SkipCeiling);
        assert_eq!(started, None);
        assert_eq!(
            w.step,
            WizardStep::Done {
                result: WizardResult {
                    noise_floor_db: -60.0,
                    quiet_db: -34.0,
                    ceiling_db: None,
                },
            }
        );
    }

    #[test]
    fn stale_complete_with_wrong_kind_during_measuring_is_ignored() {
        let mut w = Wizard::new(14);
        w.on_event(WizardEvent::Begin); // Measuring(NoiseFloor)

        let started = w.on_event(WizardEvent::Complete(QuietPoint, -34.0));
        assert_eq!(started, None);
        assert_eq!(
            w.step,
            WizardStep::Measuring { kind: NoiseFloor, progress: 0.0 },
            "wrong-kind Complete must not change state"
        );
    }

    #[test]
    fn stale_progress_during_noise_done_is_ignored() {
        let mut w = Wizard::new(14);
        w.on_event(WizardEvent::Begin);
        w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        assert_eq!(w.step, WizardStep::NoiseDone { noise_db: -60.0 });

        let started = w.on_event(WizardEvent::Progress(0.5));
        assert_eq!(started, None);
        assert_eq!(
            w.step,
            WizardStep::NoiseDone { noise_db: -60.0 },
            "Progress outside Measuring must not change state"
        );
    }

    #[test]
    fn stale_begin_during_measuring_is_ignored() {
        let mut w = Wizard::new(14);
        w.on_event(WizardEvent::Begin);
        assert_eq!(
            w.step,
            WizardStep::Measuring { kind: NoiseFloor, progress: 0.0 }
        );

        let started = w.on_event(WizardEvent::Begin);
        assert_eq!(started, None);
        assert_eq!(
            w.step,
            WizardStep::Measuring { kind: NoiseFloor, progress: 0.0 },
            "Begin outside Intro must not change state"
        );
    }

    #[test]
    fn stale_continue_during_intro_is_ignored() {
        let mut w = Wizard::new(14);
        let started = w.on_event(WizardEvent::Continue);
        assert_eq!(started, None);
        assert_eq!(w.step, WizardStep::Intro);
    }

    #[test]
    fn stale_skip_ceiling_during_noise_done_is_ignored() {
        let mut w = Wizard::new(14);
        w.on_event(WizardEvent::Begin);
        w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        assert_eq!(w.step, WizardStep::NoiseDone { noise_db: -60.0 });

        let started = w.on_event(WizardEvent::SkipCeiling);
        assert_eq!(started, None);
        assert_eq!(w.step, WizardStep::NoiseDone { noise_db: -60.0 });
    }

    #[test]
    fn cancelled_measurement_is_some_only_while_measuring() {
        let mut w = Wizard::new(14);
        assert_eq!(w.cancelled_measurement(), None); // Intro

        w.on_event(WizardEvent::Begin);
        assert_eq!(w.cancelled_measurement(), Some(NoiseFloor)); // Measuring(NoiseFloor)

        w.on_event(WizardEvent::Complete(NoiseFloor, -60.0));
        assert_eq!(w.cancelled_measurement(), None); // NoiseDone

        w.on_event(WizardEvent::Continue);
        assert_eq!(w.cancelled_measurement(), Some(QuietPoint)); // Measuring(QuietPoint)

        w.on_event(WizardEvent::Complete(QuietPoint, -34.0));
        assert_eq!(w.cancelled_measurement(), None); // QuietDone

        w.on_event(WizardEvent::Continue);
        assert_eq!(w.cancelled_measurement(), Some(Ceiling)); // Measuring(Ceiling)

        w.on_event(WizardEvent::Complete(Ceiling, -32.5)); // insane -> CeilingRetry
        assert_eq!(w.cancelled_measurement(), None); // CeilingRetry

        w.on_event(WizardEvent::Continue);
        assert_eq!(w.cancelled_measurement(), Some(Ceiling)); // Measuring(Ceiling) again

        w.on_event(WizardEvent::Complete(Ceiling, -18.0)); // sane -> Done
        assert_eq!(w.cancelled_measurement(), None); // Done
    }
}
