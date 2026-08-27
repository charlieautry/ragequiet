//! Decodes the brand's embedded tray/window PNGs into RGBA icons.
//!
//! Decoding happens once (called from `App::boot`); the results are cheap to
//! clone (`tray_icon::Icon` just clones its small RGBA `Vec<u8>`), so the app
//! rebuilds an `Icon` handle on every state change instead of re-decoding.

use crate::detector::TrayState;

const TRAY_QUIET: &[u8] = include_bytes!("../../assets/brand/tray-quiet-32.png");
const TRAY_WARNING: &[u8] = include_bytes!("../../assets/brand/tray-warning-32.png");
const TRAY_LOUD: &[u8] = include_bytes!("../../assets/brand/tray-loud-32.png");
const TRAY_OFF: &[u8] = include_bytes!("../../assets/brand/tray-off-32.png");
const WINDOW_ICON: &[u8] = include_bytes!("../../assets/brand/icon-32.png");

/// The four brand tray icons, decoded once at boot.
pub struct TrayIcons {
    pub quiet: tray_icon::Icon,
    pub warning: tray_icon::Icon,
    pub loud: tray_icon::Icon,
    pub off: tray_icon::Icon,
}

impl TrayIcons {
    pub fn load() -> Self {
        Self {
            quiet: tray_icon_from_png(TRAY_QUIET),
            warning: tray_icon_from_png(TRAY_WARNING),
            loud: tray_icon_from_png(TRAY_LOUD),
            off: tray_icon_from_png(TRAY_OFF),
        }
    }

    /// The icon for a live tray state (monitoring enabled).
    pub fn for_state(&self, state: TrayState) -> tray_icon::Icon {
        match state {
            TrayState::Quiet => self.quiet.clone(),
            TrayState::Warning => self.warning.clone(),
            TrayState::Loud => self.loud.clone(),
        }
    }
}

/// Decode PNG bytes into an RGBA8 buffer plus dimensions. Panics on a
/// malformed or unsupported embedded asset: a decode failure here means a
/// brand asset itself is broken, and that's a boot-time bug, not a
/// runtime condition to recover from.
fn decode_rgba(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("valid PNG header");
    let mut buf = vec![0u8; reader.output_buffer_size().expect("known PNG buffer size")];
    let info = reader.next_frame(&mut buf).expect("decodable PNG frame");
    let pixels = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        // Brand assets are expected to ship with alpha; expand RGB just in
        // case a future export drops it, rather than panicking outright.
        png::ColorType::Rgb => pixels
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => panic!("unsupported PNG color type for a brand asset: {other:?}"),
    };
    (rgba, info.width, info.height)
}

fn tray_icon_from_png(bytes: &[u8]) -> tray_icon::Icon {
    let (rgba, w, h) = decode_rgba(bytes);
    tray_icon::Icon::from_rgba(rgba, w, h).expect("valid tray icon buffer")
}

/// The window/taskbar icon for the settings window.
pub fn window_icon() -> iced::window::icon::Icon {
    let (rgba, w, h) = decode_rgba(WINDOW_ICON);
    iced::window::icon::from_rgba(rgba, w, h).expect("valid window icon buffer")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_32x32_rgba_with_visible_pixels(bytes: &[u8]) {
        let (rgba, w, h) = decode_rgba(bytes);
        assert_eq!(w, 32, "width");
        assert_eq!(h, 32, "height");
        assert_eq!(rgba.len(), 32 * 32 * 4, "RGBA8 buffer length");
        assert!(
            rgba.chunks_exact(4).any(|p| p[3] > 0),
            "must have at least one non-transparent pixel"
        );
    }

    #[test]
    fn all_embedded_brand_pngs_decode_to_32x32_rgba() {
        assert_32x32_rgba_with_visible_pixels(TRAY_QUIET);
        assert_32x32_rgba_with_visible_pixels(TRAY_WARNING);
        assert_32x32_rgba_with_visible_pixels(TRAY_LOUD);
        assert_32x32_rgba_with_visible_pixels(TRAY_OFF);
        assert_32x32_rgba_with_visible_pixels(WINDOW_ICON);
    }
}
