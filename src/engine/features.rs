use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Root-mean-square of a frame. 0.0 for an empty frame.
pub fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
    (sum_sq / frame.len() as f32).sqrt()
}

/// Linear RMS -> dBFS, floored at -100 dB. One log10 per frame, never per sample.
pub fn db_from_rms(rms: f32) -> f32 {
    if rms <= 1e-5 {
        return -100.0;
    }
    20.0 * rms.log10()
}

#[allow(dead_code)] // used from Task 7 on
pub fn zero_crossing_rate(frame: &[f32]) -> f32 {
    if frame.len() < 2 {
        return 0.0;
    }
    let crossings = frame
        .windows(2)
        .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
        .count();
    crossings as f32 / (frame.len() - 1) as f32
}

#[allow(dead_code)] // used from Task 7 on
#[derive(Debug, Clone, Copy)]
pub struct SpectralFeatures {
    /// 10*log10(energy 1-4 kHz / energy 100-500 Hz). Raised voice is brighter.
    pub tilt_db: f32,
    /// Fraction of 50 Hz-8 kHz energy that sits in 100-1000 Hz.
    pub speech_ratio: f32,
}

/// 512-point real FFT with preallocated buffers; analyze() never allocates.
#[allow(dead_code)] // used from Task 7 on
pub struct Spectrum {
    fft: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    power: Vec<f32>,
    bin_hz: f32,
}

#[allow(dead_code)] // used from Task 7 on
impl Spectrum {
    pub fn new(size: usize, sample_rate: f32) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(size);
        let input = fft.make_input_vec();
        let output = fft.make_output_vec();
        let power = vec![0.0; output.len()];
        Self { fft, input, output, power, bin_hz: sample_rate / size as f32 }
    }

    pub fn analyze(&mut self, frame: &[f32]) -> SpectralFeatures {
        self.input.copy_from_slice(frame);
        // process() cannot fail when buffer sizes come from the plan itself
        let _ = self.fft.process(&mut self.input, &mut self.output);
        for (p, c) in self.power.iter_mut().zip(&self.output) {
            *p = c.norm_sqr();
        }
        let low = self.band_power(100.0, 500.0);
        let high = self.band_power(1000.0, 4000.0);
        let speech = self.band_power(100.0, 1000.0);
        let total = self.band_power(50.0, 8000.0);
        const EPS: f32 = 1e-12;
        SpectralFeatures {
            tilt_db: 10.0 * ((high + EPS) / (low + EPS)).log10(),
            speech_ratio: speech / (total + EPS),
        }
    }

    fn band_power(&self, lo_hz: f32, hi_hz: f32) -> f32 {
        let lo = (lo_hz / self.bin_hz).round() as usize;
        let hi = ((hi_hz / self.bin_hz).round() as usize).min(self.power.len() - 1);
        self.power[lo..=hi].iter().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32, rate: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 / rate * freq * std::f32::consts::TAU).sin() * amp)
            .collect()
    }

    #[test]
    fn rms_of_full_scale_sine_is_0_707() {
        let s = sine(1000.0, 1.0, 16000.0, 512);
        assert!((rms(&s) - 0.707).abs() < 0.01);
    }

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&[0.0; 512]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn db_of_full_scale_sine_is_minus_3() {
        let s = sine(1000.0, 1.0, 16000.0, 512);
        assert!((db_from_rms(rms(&s)) - (-3.0)).abs() < 0.1);
    }

    #[test]
    fn db_scales_20_per_decade() {
        // amp 0.1 sine is 20 dB below amp 1.0 sine
        let loud = db_from_rms(rms(&sine(1000.0, 1.0, 16000.0, 512)));
        let soft = db_from_rms(rms(&sine(1000.0, 0.1, 16000.0, 512)));
        assert!((loud - soft - 20.0).abs() < 0.1);
    }

    #[test]
    fn db_of_silence_is_floor() {
        assert_eq!(db_from_rms(0.0), -100.0);
    }

    #[test]
    fn zcr_of_low_sine_is_low() {
        // 200 Hz at 16 kHz: 2 crossings per period, 400/sec -> 0.025
        let s = sine(200.0, 0.5, 16000.0, 512);
        let z = zero_crossing_rate(&s);
        assert!(z > 0.01 && z < 0.05, "zcr={z}");
    }

    #[test]
    fn zcr_of_alternating_signal_is_one() {
        let s: Vec<f32> = (0..512).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        assert!((zero_crossing_rate(&s) - 1.0).abs() < 0.01);
    }

    #[test]
    fn low_sine_has_negative_tilt_and_high_speech_ratio() {
        let mut sp = Spectrum::new(512, 16000.0);
        let s = sine(200.0, 0.5, 16000.0, 512);
        let f = sp.analyze(&s);
        assert!(f.tilt_db < -10.0, "tilt={}", f.tilt_db);
        assert!(f.speech_ratio > 0.8, "ratio={}", f.speech_ratio);
    }

    #[test]
    fn bright_sine_has_positive_tilt_and_low_speech_ratio() {
        let mut sp = Spectrum::new(512, 16000.0);
        let s = sine(2500.0, 0.5, 16000.0, 512);
        let f = sp.analyze(&s);
        assert!(f.tilt_db > 10.0, "tilt={}", f.tilt_db);
        assert!(f.speech_ratio < 0.2, "ratio={}", f.speech_ratio);
    }
}
