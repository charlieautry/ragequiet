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
}
