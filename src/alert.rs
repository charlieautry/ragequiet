/// Hold/cooldown state machine. Pure; caller supplies a monotonic clock in ms.
pub struct AlertGate {
    hold_ms: u64,
    cooldown_ms: u64,
    over_since: Option<u64>,
    cooldown_until: u64,
}

impl AlertGate {
    pub fn new(hold_ms: u64, cooldown_ms: u64) -> Self {
        Self {
            hold_ms,
            cooldown_ms,
            over_since: None,
            cooldown_until: 0,
        }
    }

    /// Feed the current over-threshold flag; returns true when an alert should fire.
    pub fn update(&mut self, over: bool, now_ms: u64) -> bool {
        if !over {
            self.over_since = None;
            return false;
        }
        if now_ms < self.cooldown_until {
            // Still cooling down. Latch the hold start to when cooldown ends so a
            // signal that stays over threshold through the cooldown fires as soon
            // as it's been over for hold_ms past cooldown_until, rather than
            // restarting the hold clock at whatever moment we happen to poll.
            if self.over_since.is_none() {
                self.over_since = Some(self.cooldown_until);
            }
            return false;
        }
        match self.over_since {
            None => {
                self.over_since = Some(now_ms);
                false
            }
            Some(start) if now_ms - start >= self.hold_ms => {
                self.over_since = None;
                self.cooldown_until = now_ms + self.cooldown_ms;
                true
            }
            Some(_) => false,
        }
    }
}

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const BEEP_HZ: f32 = 880.0;
const BEEP_SECS: f32 = 0.15;

/// Fire-and-forget beep on the default output device.
/// Spawns a short-lived thread; alerts are rare, so this never runs hot.
pub fn play_beep() {
    std::thread::spawn(|| {
        if let Err(e) = beep_blocking() {
            eprintln!("alert playback failed: {e}");
        }
    });
}

fn beep_blocking() -> anyhow::Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no default output device")?;
    let config = device.default_output_config()?;
    anyhow::ensure!(
        config.sample_format() == cpal::SampleFormat::F32,
        "unsupported output format {:?}",
        config.sample_format()
    );
    let rate = config.sample_rate() as f32;
    let channels = config.channels() as usize;
    let total = (rate * BEEP_SECS) as usize;
    let ramp = (rate * 0.01) as usize; // 10 ms fade in/out, no clicks
    let mut i = 0usize;

    let stream = device.build_output_stream(
        config.into(),
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for frame in data.chunks_mut(channels) {
                let s = if i < total {
                    let env = if i < ramp {
                        i as f32 / ramp as f32
                    } else if i > total - ramp {
                        (total - i) as f32 / ramp as f32
                    } else {
                        1.0
                    };
                    (i as f32 / rate * BEEP_HZ * std::f32::consts::TAU).sin() * 0.25 * env
                } else {
                    0.0
                };
                for out in frame {
                    *out = s;
                }
                i += 1;
            }
        },
        |e| eprintln!("audio output error: {e}"),
        None,
    )?;
    stream.play()?;
    std::thread::sleep(std::time::Duration::from_millis(
        (BEEP_SECS * 1000.0) as u64 + 100,
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_fire_before_hold_time() {
        let mut g = AlertGate::new(300, 3000);
        assert!(!g.update(true, 0));
        assert!(!g.update(true, 100));
        assert!(!g.update(true, 299));
    }

    #[test]
    fn fires_after_hold_time() {
        let mut g = AlertGate::new(300, 3000);
        assert!(!g.update(true, 0));
        assert!(g.update(true, 300));
    }

    #[test]
    fn a_dip_resets_the_hold() {
        let mut g = AlertGate::new(300, 3000);
        assert!(!g.update(true, 0));
        assert!(!g.update(false, 200)); // dropped under threshold
        assert!(!g.update(true, 250)); // hold restarts here
        assert!(!g.update(true, 500));
        assert!(g.update(true, 550));
    }

    #[test]
    fn cooldown_blocks_refire() {
        let mut g = AlertGate::new(300, 3000);
        g.update(true, 0);
        assert!(g.update(true, 300));
        assert!(!g.update(true, 700)); // still over, but cooling down
        assert!(!g.update(true, 3200)); // cooldown ends at 3300
        assert!(g.update(true, 3600)); // over continuously since 3300 for 300ms
    }
}
