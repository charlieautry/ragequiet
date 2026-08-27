use std::sync::atomic::{AtomicU32, Ordering};

/// Lock-free per-frame readouts for the meter. Written by the audio callback,
/// read by the UI at ~12 fps while the settings window is open.
#[derive(Default)]
pub struct SharedLevels {
    level_db: AtomicU32,     // f32 bits
    threshold_db: AtomicU32, // f32 bits
    peak_db: AtomicU32,      // f32 bits, decaying ~3 s ghost peak
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

/// UI -> audio-callback commands. Drained with try_recv each frame (non-blocking).
// The shared `Set` prefix is the point: every variant is an imperative write
// into the live detector, and the names read correctly at the call site.
#[allow(clippy::enum_variant_names)]
pub enum Command {
    SetTuning(crate::engine::Tuning),
    SetGate { hold_ms: u64, cooldown_ms: u64 },
    SetTestMode(bool),
    SetEnabledIconBaseline, // resume(): re-announce color after enable toggle
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
}
