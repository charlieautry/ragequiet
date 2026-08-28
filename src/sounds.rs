//! Built-in alert sound bank: three synthesized in code plus nine recorded
//! sounds embedded at compile time. See sounds/LICENSES.md for provenance.

pub const SOUND_RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinSound {
    SoftBeep,
    DoubleBeep,
    Chime,
    AlarmClock,
    Barking,
    DoorKnock,
    ShutUp,
    Yelp,
    Gong,
    Rahhh,
    Shh,
    SonarPing,
}

pub const ALL: [BuiltinSound; 12] = [
    BuiltinSound::SoftBeep,
    BuiltinSound::DoubleBeep,
    BuiltinSound::Chime,
    BuiltinSound::AlarmClock,
    BuiltinSound::Barking,
    BuiltinSound::DoorKnock,
    BuiltinSound::ShutUp,
    BuiltinSound::Yelp,
    BuiltinSound::Gong,
    BuiltinSound::Rahhh,
    BuiltinSound::Shh,
    BuiltinSound::SonarPing,
];

/// A built-in sound's underlying data: either a pure-code synth function
/// (called once at boot and cached) or the raw bytes of an embedded WAV file
/// (decoded once at boot via `decode::decode_bytes`).
pub enum SoundData {
    Synth(fn() -> Vec<f32>),
    Wav(&'static [u8]),
}

impl BuiltinSound {
    pub fn label(&self) -> &'static str {
        match self {
            BuiltinSound::SoftBeep => "Soft beep",
            BuiltinSound::DoubleBeep => "Double beep",
            BuiltinSound::Chime => "Chime",
            BuiltinSound::AlarmClock => "Alarm clock",
            BuiltinSound::Barking => "Barking",
            BuiltinSound::DoorKnock => "Door knock",
            BuiltinSound::ShutUp => "Shut up",
            BuiltinSound::Yelp => "Yelp",
            BuiltinSound::Gong => "Gong",
            BuiltinSound::Rahhh => "Rahhh",
            BuiltinSound::Shh => "Shh",
            BuiltinSound::SonarPing => "Sonar ping",
        }
    }

    /// This sound's underlying data — a synth function for the three
    /// original built-ins, or the embedded WAV bytes for a recorded one.
    pub fn data(&self) -> SoundData {
        match self {
            BuiltinSound::SoftBeep => SoundData::Synth(soft_beep),
            BuiltinSound::DoubleBeep => SoundData::Synth(double_beep),
            BuiltinSound::Chime => SoundData::Synth(chime),
            BuiltinSound::AlarmClock => SoundData::Wav(include_bytes!("../assets/sounds/alarmclock.wav")),
            BuiltinSound::Barking => SoundData::Wav(include_bytes!("../assets/sounds/barking.wav")),
            BuiltinSound::DoorKnock => SoundData::Wav(include_bytes!("../assets/sounds/doorknocking.wav")),
            BuiltinSound::ShutUp => SoundData::Wav(include_bytes!("../assets/sounds/femaleshutup.wav")),
            BuiltinSound::Yelp => SoundData::Wav(include_bytes!("../assets/sounds/femaleyelp.wav")),
            BuiltinSound::Gong => SoundData::Wav(include_bytes!("../assets/sounds/gong.wav")),
            BuiltinSound::Rahhh => SoundData::Wav(include_bytes!("../assets/sounds/rahhh.wav")),
            BuiltinSound::Shh => SoundData::Wav(include_bytes!("../assets/sounds/shh.wav")),
            BuiltinSound::SonarPing => SoundData::Wav(include_bytes!("../assets/sounds/sonarping.wav")),
        }
    }
}

/// Render a *synthesized* sound as mono f32 samples at [`SOUND_RATE`].
/// Called once at startup per synth sound and cached; not
/// performance-sensitive. Panics if `sound` is a `SoundData::Wav` variant —
/// callers should branch on [`BuiltinSound::data`] instead of calling this
/// unconditionally (see `SoundCache::render_all` in `src/app.rs`).
pub fn render(sound: BuiltinSound) -> Vec<f32> {
    match sound.data() {
        SoundData::Synth(f) => f(),
        SoundData::Wav(_) => panic!("{sound:?} is a recorded sound, not a synth — decode its WAV instead"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode;

    fn max_abs(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    fn rms(samples: &[f32]) -> f32 {
        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        (sum_sq / samples.len() as f32).sqrt()
    }

    /// Every builtin's samples, regardless of whether it's synthesized or
    /// decoded from an embedded WAV — the single source both constraint
    /// tests and the determinism test pull from.
    fn samples_for(sound: BuiltinSound) -> Vec<f32> {
        match sound.data() {
            SoundData::Synth(_) => render(sound),
            SoundData::Wav(bytes) => decode::decode_bytes(bytes, Some("wav")).expect("embedded WAV must decode").0,
        }
    }

    #[test]
    fn every_builtin_sound_satisfies_shared_constraints() {
        for sound in ALL {
            let samples = samples_for(sound);
            assert!(!samples.is_empty(), "{sound:?} rendered no samples");
            // Longest recorded clip (gong) runs ~3.0s; give headroom to 4s.
            assert!(
                samples.len() < 4 * SOUND_RATE as usize,
                "{sound:?} is too long: {} samples",
                samples.len()
            );
            // Recordings are normalized to -3 dBFS (~0.708); 0.9 still holds
            // comfortably above that with margin for peaks the normalization
            // pass didn't fully flatten.
            assert!(
                max_abs(&samples) <= 0.9,
                "{sound:?} clips: max abs {}",
                max_abs(&samples)
            );
            // Recorded clips' 5-15ms fades measure well under 0.002 at the
            // endpoints in practice (verified via decode); 0.01 (the same
            // bound the pure-synth envelopes hit exactly) holds comfortably
            // for both without permitting an audible click.
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
            let a = samples_for(sound);
            let b = samples_for(sound);
            assert_eq!(a, b, "{sound:?} did not render/decode identically twice");
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
