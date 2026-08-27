use std::sync::atomic::{AtomicU32, Ordering};

/// Lock-free per-frame readouts for the meter. Written by the audio callback,
/// read by the UI at ~12 fps while the settings window is open.
pub struct SharedLevels {
    level_db: AtomicU32,     // f32 bits
    threshold_db: AtomicU32, // f32 bits
    peak_db: AtomicU32,      // f32 bits, decaying ~3 s ghost peak
}

impl Default for SharedLevels {
    /// Silence idiom (-100 dBFS) for level/peak, not the bit-pattern-zero
    /// (0.0 dBFS, i.e. full scale) that `#[derive(Default)]` would give —
    /// before the audio thread's first store, a fresh load must read as
    /// silent rather than a full-width red bar. The threshold has no honest
    /// silent value, so it starts NaN; the meter already guards a non-finite
    /// threshold as "no threshold yet".
    fn default() -> Self {
        Self {
            level_db: AtomicU32::new((-100.0f32).to_bits()),
            threshold_db: AtomicU32::new(f32::NAN.to_bits()),
            peak_db: AtomicU32::new((-100.0f32).to_bits()),
        }
    }
}

impl SharedLevels {
    pub fn store(&self, level_db: f32, threshold_db: f32, peak_db: f32) {
        self.level_db.store(level_db.to_bits(), Ordering::Relaxed);
        self.threshold_db.store(threshold_db.to_bits(), Ordering::Relaxed);
        self.peak_db.store(peak_db.to_bits(), Ordering::Relaxed);
    }

    pub fn load(&self) -> (f32, f32, f32) {
        (
            f32::from_bits(self.level_db.load(Ordering::Relaxed)),
            f32::from_bits(self.threshold_db.load(Ordering::Relaxed)),
            f32::from_bits(self.peak_db.load(Ordering::Relaxed)),
        )
    }
}

/// Which calibration step is being measured; drives which `Measurement`
/// constructor the detector uses and what completion event flows back.
// Constructed from app.rs once the calibration wizard (Phase 2c Task 2/3)
// sends `Command::StartMeasurement`; until then only the detector's own
// tests build these.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasurementKind {
    NoiseFloor,
    QuietPoint,
    Ceiling,
}

/// UI -> audio-callback commands. Drained with try_recv each frame (non-blocking).
// The shared `Set` prefix is the point: every variant is an imperative write
// into the live detector, and the names read correctly at the call site.
#[allow(clippy::enum_variant_names)]
pub enum Command {
    SetTuning(crate::engine::Tuning),
    SetGate { hold_ms: u64, cooldown_ms: u64 },
    SetTestMode(bool),
    SetEnabledIconBaseline, // resume(): re-announce color after enable toggle
    // Sent from app.rs starting with the calibration wizard (Phase 2c Task 2/3).
    #[allow(dead_code)]
    StartMeasurement(MeasurementKind),
    #[allow(dead_code)]
    CancelMeasurement,
}

pub type CommandTx = std::sync::mpsc::Sender<Command>;
pub type CommandRx = std::sync::mpsc::Receiver<Command>;

pub fn command_channel() -> (CommandTx, CommandRx) {
    std::sync::mpsc::channel()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_levels_round_trip() {
        let s = SharedLevels::default();
        s.store(-42.5, -30.0, -12.25);
        assert_eq!(s.load(), (-42.5, -30.0, -12.25));
    }

    #[test]
    fn shared_levels_default_reads_as_silent_with_no_threshold() {
        // Before the audio thread's first store, a fresh load must not read
        // as 0.0 dBFS (full scale) — that would paint a full red bar.
        let s = SharedLevels::default();
        let (level_db, threshold_db, peak_db) = s.load();
        assert_eq!(level_db, -100.0);
        assert!(threshold_db.is_nan());
        assert_eq!(peak_db, -100.0);
    }
}
