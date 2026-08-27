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

/// Name of the default input device, for config lookups.
pub fn default_input_name() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .map(|d| d.to_string())
}

/// Opens the default input device and calls `on_frame` with mono 16 kHz frames.
/// Returns the stream; caller must keep it alive and call .play().
pub fn start_input(
    mut on_frame: impl FnMut(&[f32]) + Send + 'static,
) -> anyhow::Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("no default input device")?;
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
    Ok(stream)
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

    #[test]
    fn non_integer_ratio_produces_expected_frame_count() {
        // 44.1 kHz -> 16 kHz: 44100 samples in one second -> ~16000 out -> 31 full frames
        let mut fb = FrameBuilder::new(44_100, 1);
        let mut frames = 0;
        fb.push(&vec![0.0f32; 44_100], |_| frames += 1);
        assert_eq!(frames, 31);
    }
}
