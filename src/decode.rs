//! Custom alert sound file decoding (WAV/MP3) via `symphonia`, for the
//! user-chosen file path in `Config::AlertSound::Custom`. Decoded once at
//! selection/startup into an in-memory mono f32 buffer at the file's native
//! sample rate; the player converts it to the output rate the same way it
//! converts the synthesized built-ins.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;

/// Longer than this, an alert sound is truncated rather than rejected — a
/// misconfigured 3-minute file just gets cut short, not refused outright.
const MAX_DURATION: Duration = Duration::from_secs(10);

/// Decodes a WAV or MP3 file into mono f32 samples plus its native sample
/// rate, averaging channels down to mono and truncating at [`MAX_DURATION`].
/// Errors on an unreadable/unsupported/corrupt file, or one with no decodable
/// audio at all.
pub fn decode_file(path: &Path) -> anyhow::Result<(Vec<f32>, u32)> {
    let file = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_string);
    decode_source(Box::new(file), ext.as_deref())
}

/// Decodes an in-memory audio buffer (WAV/MP3) the same way [`decode_file`]
/// decodes a file — used for the sound pack embedded via `include_bytes!` in
/// `src/sounds.rs`. `hint_ext` (e.g. `Some("wav")`) helps the format prober
/// the same way a file extension does; pass `None` if unknown.
pub fn decode_bytes(bytes: &'static [u8], hint_ext: Option<&str>) -> anyhow::Result<(Vec<f32>, u32)> {
    decode_source(Box::new(std::io::Cursor::new(bytes)), hint_ext)
}

fn decode_source(source: Box<dyn MediaSource>, hint_ext: Option<&str>) -> anyhow::Result<(Vec<f32>, u32)> {
    let mss = MediaSourceStream::new(source, Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = hint_ext {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .context("unrecognized or corrupt audio file")?;

    let track = format.default_track(TrackType::Audio).context("no audio track in file")?;
    let track_id = track.id;
    let codec_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .context("no usable audio codec parameters")?
        .clone();
    let sample_rate = codec_params.sample_rate.context("audio track has no sample rate")?;

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&codec_params, &AudioDecoderOptions::default())
        .context("unsupported audio codec")?;

    let cap = sample_rate as usize * MAX_DURATION.as_secs() as usize;
    let mut samples: Vec<f32> = Vec::new();
    let mut interleaved: Vec<f32> = Vec::new();

    loop {
        if samples.len() >= cap {
            break;
        }
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break, // end of stream
            Err(SymphoniaError::ResetRequired) => break,
            Err(_) => break, // unrecoverable demux error: stop with whatever was decoded so far
        };
        if packet.track_id != track_id {
            continue;
        }
        let audio_buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            // A handful of malformed packets mid-file shouldn't fail the
            // whole decode; only "we got nothing at all" is an error.
            Err(_) => continue,
        };

        let channels = audio_buf.spec().channels().count().max(1);
        interleaved.resize(audio_buf.samples_interleaved(), 0.0f32);
        audio_buf.copy_to_slice_interleaved(&mut interleaved);
        for chunk in interleaved.chunks_exact(channels) {
            samples.push(chunk.iter().sum::<f32>() / channels as f32);
            if samples.len() >= cap {
                break;
            }
        }
    }

    if samples.is_empty() {
        anyhow::bail!("no audio samples decoded");
    }

    Ok((samples, sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal 16-bit PCM mono WAV file in memory: RIFF/WAVE header,
    /// a `fmt ` chunk, and a `data` chunk containing a short sine tone. No
    /// test asset files needed.
    fn make_test_wav(sample_rate: u32, tone_hz: f32, num_samples: usize) -> Vec<u8> {
        let bits_per_sample: u16 = 16;
        let channels: u16 = 1;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        let data_len = num_samples * 2; // 2 bytes per i16 sample

        let mut samples = Vec::with_capacity(data_len);
        for i in 0..num_samples {
            let t = i as f32 / sample_rate as f32;
            let value = (t * tone_hz * std::f32::consts::TAU).sin() * 0.5;
            let sample = (value * i16::MAX as f32) as i16;
            samples.extend_from_slice(&sample.to_le_bytes());
        }

        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");

        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits_per_sample.to_le_bytes());

        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_len as u32).to_le_bytes());
        wav.extend_from_slice(&samples);

        wav
    }

    fn temp_wav_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ragequiet-test-decode-{tag}-{}.wav", std::process::id()))
    }

    #[test]
    fn decodes_a_short_wav_to_the_expected_rate_and_length() {
        let path = temp_wav_path("short");
        let sample_rate = 8_000u32;
        let num_samples = 400; // 50 ms at 8 kHz
        let wav = make_test_wav(sample_rate, 440.0, num_samples);
        std::fs::write(&path, &wav).unwrap();

        let (samples, rate) = decode_file(&path).expect("valid wav must decode");
        assert_eq!(rate, sample_rate);
        assert_eq!(samples.len(), num_samples);
        // First sample of a sine starting at phase 0 is ~0.
        assert!(samples[0].abs() < 0.05, "unexpected first sample {}", samples[0]);
        // Somewhere in the tone there should be real amplitude, not silence.
        let max_abs = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(max_abs > 0.3, "decoded tone looks silent: max abs {max_abs}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn truncates_audio_longer_than_the_ten_second_cap() {
        let path = temp_wav_path("long");
        let sample_rate = 4_000u32; // small rate keeps the synthetic file tiny
        let num_samples = sample_rate as usize * 15; // 15 s of audio
        let wav = make_test_wav(sample_rate, 220.0, num_samples);
        std::fs::write(&path, &wav).unwrap();

        let (samples, rate) = decode_file(&path).expect("valid wav must decode");
        assert_eq!(rate, sample_rate);
        assert_eq!(samples.len(), sample_rate as usize * 10, "must truncate to the 10s cap");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let path = temp_wav_path("corrupt");
        std::fs::write(&path, b"this is not a wav file at all, just junk bytes").unwrap();

        let result = decode_file(&path);
        assert!(result.is_err(), "corrupt file must not decode successfully");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_is_an_error() {
        let path = temp_wav_path("missing-does-not-exist");
        std::fs::remove_file(&path).ok(); // ensure it really doesn't exist
        assert!(decode_file(&path).is_err());
    }
}
