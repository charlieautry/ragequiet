use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait};

use crate::engine::{FRAME_SIZE, SAMPLE_RATE};

/// Converts interleaved device samples into mono FRAME_SIZE frames at SAMPLE_RATE.
/// All buffers preallocated; push() never allocates.
pub struct FrameBuilder {
    channels: usize,
    step: f32, // input samples per output sample
    acc: f32,
    frame: Vec<f32>,
}

impl FrameBuilder {
    pub fn new(input_rate: u32, channels: u16) -> Self {
        Self {
            channels: channels as usize,
            step: input_rate as f32 / SAMPLE_RATE as f32,
            acc: 0.0,
            frame: Vec::with_capacity(FRAME_SIZE),
        }
    }

    pub fn push(&mut self, interleaved: &[f32], mut on_frame: impl FnMut(&[f32])) {
        for chunk in interleaved.chunks_exact(self.channels) {
            let mono = chunk.iter().sum::<f32>() / self.channels as f32;
            self.acc += 1.0;
            if self.acc >= self.step {
                self.acc -= self.step;
                self.frame.push(mono);
                if self.frame.len() == FRAME_SIZE {
                    on_frame(&self.frame);
                    self.frame.clear();
                }
            }
        }
    }
}

/// Name of the default input device. Used to notice that the OS default has
/// moved out from under a stream that was opened on it (see
/// `App::default_device_moved`), without enumerating every device.
pub fn default_input_name() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .map(|d| d.to_string())
}

/// Pure half of input-device resolution: where a preferred device name lands
/// in an enumerated list. `None` — no preference, or a preference nothing
/// matches (the device was unplugged since it was configured) — means "use
/// the system default". Matching is exact: cpal names are driver-supplied
/// text, so a near-miss is a different device, not this one.
fn preferred_device_index(preferred: Option<&str>, available: &[String]) -> Option<usize> {
    let preferred = preferred?;
    available.iter().position(|name| name == preferred)
}

/// The device a preference resolves to, falling back to the system default
/// whenever the named one isn't present.
fn resolve_input_device(host: &cpal::Host, preferred: Option<&str>) -> Option<cpal::Device> {
    if preferred.is_some()
        && let Ok(devices) = host.input_devices()
    {
        let devices: Vec<cpal::Device> = devices.collect();
        let names: Vec<String> = devices.iter().map(|d| d.to_string()).collect();
        if let Some(index) = preferred_device_index(preferred, &names) {
            return devices.into_iter().nth(index);
        }
    }
    host.default_input_device()
}

/// Opens an input device — the one named by `preferred`, or the system
/// default when that is `None` or names a device that isn't present — and
/// calls `on_frame` with mono 16 kHz frames.
///
/// Returns the stream alongside the *resolved* device's display name: the
/// caller keys calibration off that name, and taking it from the very device
/// that was opened is what keeps the two from disagreeing (re-resolving the
/// name separately could race a device change between the two lookups).
/// Caller must keep the stream alive and call `.play()`.
pub fn start_input_on(
    preferred: Option<&str>,
    mut on_frame: impl FnMut(&[f32]) + Send + 'static,
) -> anyhow::Result<(cpal::Stream, String)> {
    let host = cpal::default_host();
    let device = resolve_input_device(&host, preferred).context("no input device")?;
    let device_name = device.to_string();
    let config = device
        .default_input_config()
        .context("no default input config")?;
    let mut builder = FrameBuilder::new(config.sample_rate(), config.channels());
    let err_fn = |e| eprintln!("audio input error: {e}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                builder.push(data, &mut on_frame);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => {
            let mut scratch = vec![0.0f32; 4096];
            device.build_input_stream(
                config.into(),
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    for chunk in data.chunks(4096) {
                        for (d, s) in scratch.iter_mut().zip(chunk) {
                            *d = *s as f32 / 32768.0;
                        }
                        builder.push(&scratch[..chunk.len()], &mut on_frame);
                    }
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let mut scratch = vec![0.0f32; 4096];
            device.build_input_stream(
                config.into(),
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    for chunk in data.chunks(4096) {
                        for (d, s) in scratch.iter_mut().zip(chunk) {
                            *d = (*s as f32 - 32768.0) / 32768.0;
                        }
                        builder.push(&scratch[..chunk.len()], &mut on_frame);
                    }
                },
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("unsupported sample format {other:?}"),
    };
    Ok((stream, device_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_is_averaged_to_mono() {
        // 16 kHz stereo in (no resampling): L=0.8, R=0.2 -> mono 0.5
        let mut fb = FrameBuilder::new(SAMPLE_RATE, 2);
        let interleaved: Vec<f32> = std::iter::repeat_n([0.8f32, 0.2f32], FRAME_SIZE)
            .flatten()
            .collect();
        let mut frames = 0;
        fb.push(&interleaved, |frame| {
            frames += 1;
            assert!(frame.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        });
        assert_eq!(frames, 1);
    }

    #[test]
    fn decimates_48k_to_16k() {
        // 512*3 mono samples at 48 kHz -> exactly one 512-sample frame
        let mut fb = FrameBuilder::new(48_000, 1);
        let input = vec![0.1f32; FRAME_SIZE * 3];
        let mut frames = 0;
        fb.push(&input, |frame| {
            frames += 1;
            assert_eq!(frame.len(), FRAME_SIZE);
        });
        assert_eq!(frames, 1);
    }

    #[test]
    fn partial_input_carries_over_between_pushes() {
        let mut fb = FrameBuilder::new(SAMPLE_RATE, 1);
        let mut frames = 0;
        fb.push(&vec![0.0f32; 300], |_| frames += 1);
        assert_eq!(frames, 0);
        fb.push(&vec![0.0f32; 212], |_| frames += 1);
        assert_eq!(frames, 1);
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_preference_resolves_to_the_default_device() {
        assert_eq!(preferred_device_index(None, &names(&["Mic A", "Mic B"])), None);
    }

    #[test]
    fn a_named_preference_picks_its_exact_entry() {
        assert_eq!(preferred_device_index(Some("Mic B"), &names(&["Mic A", "Mic B"])), Some(1));
    }

    #[test]
    fn a_missing_preference_falls_back_to_the_default_device() {
        assert_eq!(preferred_device_index(Some("Unplugged"), &names(&["Mic A", "Mic B"])), None);
        assert_eq!(preferred_device_index(Some("Mic A"), &[]), None);
    }

    #[test]
    fn preference_matching_is_exact_not_fuzzy() {
        // Device names are driver-supplied text; a near-miss is a different
        // device, not this one.
        assert_eq!(preferred_device_index(Some("Mic"), &names(&["Mic A"])), None);
        assert_eq!(preferred_device_index(Some("mic a"), &names(&["Mic A"])), None);
    }

    #[test]
    fn duplicate_device_names_resolve_to_the_first_match() {
        assert_eq!(preferred_device_index(Some("Mic A"), &names(&["Mic A", "Mic A"])), Some(0));
    }

    #[test]
    fn non_integer_ratio_produces_expected_frame_count() {
        // 44.1 kHz -> 16 kHz: 44100 samples in one second -> ~16000 out -> 31 full frames
        let mut fb = FrameBuilder::new(44_100, 1);
        let mut frames = 0;
        fb.push(&vec![0.0f32; 44_100], |_| frames += 1);
        assert_eq!(frames, 31);
    }
}
