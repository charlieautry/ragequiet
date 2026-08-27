//! Synthesized alert sound bank.
//!
//! Every built-in sound is generated in code — no third-party audio, nothing
//! to attribute (spec §6). See sounds/LICENSES.md.

pub const SOUND_RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinSound {
    SoftBeep,
    DoubleBeep,
    Chime,
    Knock,
    Shh,
    Boing,
}

pub const ALL: [BuiltinSound; 6] = [
    BuiltinSound::SoftBeep,
    BuiltinSound::DoubleBeep,
    BuiltinSound::Chime,
    BuiltinSound::Knock,
    BuiltinSound::Shh,
    BuiltinSound::Boing,
];

impl BuiltinSound {
    pub fn label(&self) -> &'static str {
        match self {
            BuiltinSound::SoftBeep => "Soft beep",
            BuiltinSound::DoubleBeep => "Double beep",
            BuiltinSound::Chime => "Chime",
            BuiltinSound::Knock => "Knock",
            BuiltinSound::Shh => "Shh",
            BuiltinSound::Boing => "Boing",
        }
    }
}

/// Render a sound as mono f32 samples at [`SOUND_RATE`]. Called once at
/// startup per sound and cached; not performance-sensitive.
pub fn render(sound: BuiltinSound) -> Vec<f32> {
    match sound {
        BuiltinSound::SoftBeep => soft_beep(),
        BuiltinSound::DoubleBeep => double_beep(),
        BuiltinSound::Chime => chime(),
        BuiltinSound::Knock => knock(),
        BuiltinSound::Shh => shh(),
        BuiltinSound::Boing => boing(),
    }
}

use std::f32::consts::TAU;

/// Convert a duration in milliseconds to a sample count at [`SOUND_RATE`].
fn ms(duration_ms: f32) -> usize {
    ((SOUND_RATE as f32) * duration_ms / 1000.0).round() as usize
}

/// Linear fade envelope: ramps 0->1 over the first `attack` samples, holds
/// at 1, then ramps 1->0 over the final `release` samples of a `total`
/// sample buffer. Used to guarantee click-free starts/ends.
fn fade_env(i: usize, total: usize, attack: usize, release: usize) -> f32 {
    if i < attack {
        i as f32 / attack as f32
    } else if i >= total - release {
        (total - i) as f32 / release as f32
    } else {
        1.0
    }
}

/// Small, deterministic, const-seeded PRNG (no `rand` dependency) used for
/// the noise-based sounds (Knock, Shh).
struct Xorshift32(u32);

impl Xorshift32 {
    fn new(seed: u32) -> Self {
        // xorshift32 has a fixed point at 0; any nonzero seed works.
        Self(if seed == 0 { 1 } else { seed })
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Next sample of white noise in roughly [-1, 1).
    fn next_f32(&mut self) -> f32 {
        (self.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn soft_beep() -> Vec<f32> {
    const HZ: f32 = 880.0;
    let total = ms(150.0);
    let ramp = ms(10.0);
    let rate = SOUND_RATE as f32;
    (0..total)
        .map(|i| {
            let env = fade_env(i, total, ramp, ramp);
            (i as f32 / rate * HZ * TAU).sin() * 0.5 * env
        })
        .collect()
}

fn double_beep() -> Vec<f32> {
    const HZ: f32 = 660.0;
    let beep = ms(120.0);
    let ramp = ms(10.0);
    let gap = ms(80.0);
    let total = beep * 2 + gap;
    let rate = SOUND_RATE as f32;
    (0..total)
        .map(|i| {
            let (phase_i, in_beep) = if i < beep {
                (i, true)
            } else if i < beep + gap {
                (0, false)
            } else {
                (i - beep - gap, true)
            };
            if !in_beep {
                return 0.0;
            }
            let env = fade_env(phase_i, beep, ramp, ramp);
            (phase_i as f32 / rate * HZ * TAU).sin() * 0.5 * env
        })
        .collect()
}

/// 523.25 Hz (C5) fundamental plus decaying 2nd/3rd harmonics, sharp attack,
/// exponential ring-out. A hard 700 ms cutoff with a 180 ms decay constant
/// would leave the tail around 0.012 at peak 0.6 (still audible, failing the
/// no-click bound), so the buffer runs to 900 ms and an explicit 15 ms
/// release fade forces the true end to zero regardless of the exponential's
/// residual value.
fn chime() -> Vec<f32> {
    const F0: f32 = 523.25;
    let attack = ms(5.0);
    let total = ms(900.0);
    let tail_fade = ms(15.0);
    let tau = 0.18; // seconds
    let rate = SOUND_RATE as f32;
    let peak = 0.6f32;
    (0..total)
        .map(|i| {
            let t = i as f32 / rate;
            let raw = (t * F0 * TAU).sin()
                + 0.4 * (t * F0 * 2.0 * TAU).sin()
                + 0.2 * (t * F0 * 3.0 * TAU).sin();
            // 1.0 + 0.4 + 0.2 = 1.6 is the worst-case combined amplitude;
            // normalizing by it keeps the harmonics within [-1, 1].
            let harmonics = raw / 1.6;
            let decay = if i < attack {
                i as f32 / attack as f32
            } else {
                let dt = (i - attack) as f32 / rate;
                (-dt / tau).exp()
            };
            let mut s = harmonics * peak * decay;
            if i >= total - tail_fade {
                s *= (total - i) as f32 / tail_fade as f32;
            }
            s
        })
        .collect()
}

/// Envelope for one Knock burst: 0 before `start`, a linear attack, then an
/// unbounded exponential decay (no hard window cutoff — cutting off the
/// decay abruptly mid-buffer would itself click).
fn burst_env(i: usize, start: usize, attack: usize, tau: f32, rate: f32) -> f32 {
    if i < start {
        return 0.0;
    }
    let j = i - start;
    if j < attack {
        j as f32 / attack as f32
    } else {
        let dt = (j - attack) as f32 / rate;
        (-dt / tau).exp()
    }
}

/// Two thuds of low-passed noise, second starting 120 ms after the first.
fn knock() -> Vec<f32> {
    let burst_gap = ms(120.0); // start-to-start spacing between the two thuds
    let burst_len = ms(60.0);
    let total = burst_gap + burst_len;
    let attack = ms(2.0);
    let tau = 0.025; // seconds
    let tail_fade = ms(10.0);
    let peak = 0.7f32;
    let rate = SOUND_RATE as f32;
    let lp_a = 0.15f32; // one-pole low-pass coefficient (thud character)
    let mut rng = Xorshift32::new(0x9E37_79B9);
    let mut lp = 0.0f32;
    (0..total)
        .map(|i| {
            let x = rng.next_f32();
            lp += lp_a * (x - lp);
            let env = burst_env(i, 0, attack, tau, rate) + burst_env(i, burst_gap, attack, tau, rate);
            let mut s = lp * peak * env;
            if i >= total - tail_fade {
                s *= (total - i) as f32 / tail_fade as f32;
            }
            s
        })
        .collect()
}

/// 400 ms of high-passed noise under a slow attack/sustain/release envelope.
fn shh() -> Vec<f32> {
    let total = ms(400.0);
    let attack = ms(80.0);
    let release = ms(150.0);
    let peak = 0.35f32;
    let hp_a = 0.95f32; // one-pole high-pass coefficient
    let mut rng = Xorshift32::new(0x1234_5678);
    let mut y = 0.0f32;
    let mut x_prev = 0.0f32;
    (0..total)
        .map(|i| {
            let x = rng.next_f32();
            y = hp_a * (y + x - x_prev);
            x_prev = x;
            let env = fade_env(i, total, attack, release);
            y * peak * env
        })
        .collect()
}

/// 300 ms sine sweeping 400 -> 150 Hz (exponential glide) with an 8 Hz,
/// +/-10% vibrato on the instantaneous frequency. Phase is accumulated
/// incrementally (phase += 2*pi*f(t)/rate) rather than evaluated as
/// sin(2*pi*f(t)*t), which would produce chirp artifacts under a
/// time-varying frequency.
fn boing() -> Vec<f32> {
    let total = ms(300.0);
    let attack = ms(5.0);
    let tail_fade = ms(20.0);
    let rate = SOUND_RATE as f32;
    let f0 = 400.0f32;
    let f1 = 150.0f32;
    let duration = total as f32 / rate;
    let vibrato_hz = 8.0f32;
    let peak = 0.6f32;
    let tau = 0.1f32; // amplitude decay time constant (seconds)
    let mut phase = 0.0f32;
    (0..total)
        .map(|i| {
            let t = i as f32 / rate;
            let glide = f0 * (f1 / f0).powf(t / duration);
            let vibrato = 1.0 + 0.1 * (TAU * vibrato_hz * t).sin();
            let freq = glide * vibrato;
            phase += TAU * freq / rate;
            let attack_env = if i < attack {
                i as f32 / attack as f32
            } else {
                1.0
            };
            let decay_env = (-t / tau).exp();
            let mut s = phase.sin() * peak * attack_env * decay_env;
            if i >= total - tail_fade {
                s *= (total - i) as f32 / tail_fade as f32;
            }
            s
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn max_abs(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    fn rms(samples: &[f32]) -> f32 {
        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        (sum_sq / samples.len() as f32).sqrt()
    }

    #[test]
    fn every_builtin_sound_satisfies_shared_constraints() {
        for sound in ALL {
            let samples = render(sound);
            assert!(!samples.is_empty(), "{sound:?} rendered no samples");
            assert!(
                samples.len() < 2 * SOUND_RATE as usize,
                "{sound:?} is too long: {} samples",
                samples.len()
            );
            assert!(
                max_abs(&samples) <= 0.9,
                "{sound:?} clips: max abs {}",
                max_abs(&samples)
            );
            assert!(
                samples[0].abs() < 0.01,
                "{sound:?} starts with a click: first sample {}",
                samples[0]
            );
            assert!(
                samples[samples.len() - 1].abs() < 0.01,
                "{sound:?} ends with a click: last sample {}",
                samples[samples.len() - 1]
            );
            assert!(
                rms(&samples) > 0.01,
                "{sound:?} is nearly silent: rms {}",
                rms(&samples)
            );
        }
    }

    #[test]
    fn rendering_is_deterministic() {
        for sound in ALL {
            let a = render(sound);
            let b = render(sound);
            assert_eq!(a, b, "{sound:?} did not render identically twice");
        }
    }

    #[test]
    fn labels_are_distinct() {
        let labels: Vec<&str> = ALL.iter().map(|s| s.label()).collect();
        for (i, a) in labels.iter().enumerate() {
            for (j, b) in labels.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "duplicate label {a:?}");
                }
            }
        }
    }
}
