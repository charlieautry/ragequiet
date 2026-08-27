use crate::engine::features::SpectralFeatures;

/// Cheap VAD: speech has a low-to-moderate zero-crossing rate and most of its
/// energy in the 100-1000 Hz band. Keyboard/mouse clicks are broadband and spiky.
pub fn is_voiced(zcr: f32, features: &SpectralFeatures) -> bool {
    (0.01..=0.35).contains(&zcr) && features.speech_ratio > 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_like_input_is_voiced() {
        let f = SpectralFeatures { tilt_db: -14.0, speech_ratio: 0.9 };
        assert!(is_voiced(0.03, &f));
    }

    #[test]
    fn broadband_click_is_rejected_by_zcr() {
        let f = SpectralFeatures { tilt_db: 5.0, speech_ratio: 0.6 };
        assert!(!is_voiced(0.7, &f));
    }

    #[test]
    fn hiss_is_rejected_by_band_ratio() {
        let f = SpectralFeatures { tilt_db: 12.0, speech_ratio: 0.1 };
        assert!(!is_voiced(0.2, &f));
    }
}
