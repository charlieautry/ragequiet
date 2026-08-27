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

    /// Dotted variants of the same four icons, composited with the
    /// calibration-incomplete marker (top-right yellow dot).
    fn load_dotted() -> Self {
        Self {
            quiet: tray_icon_from_png_dotted(TRAY_QUIET),
            warning: tray_icon_from_png_dotted(TRAY_WARNING),
            loud: tray_icon_from_png_dotted(TRAY_LOUD),
            off: tray_icon_from_png_dotted(TRAY_OFF),
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

/// Both tray icon variants, decoded once at boot; the app picks `dotted`
/// whenever the active device's calibration is incomplete.
pub struct TrayIconSets {
    pub plain: TrayIcons,
    pub dotted: TrayIcons,
}

impl TrayIconSets {
    pub fn load() -> Self {
        Self { plain: TrayIcons::load(), dotted: TrayIcons::load_dotted() }
    }
}

/// Calibration-incomplete marker: opaque #ffd23f (brand palette), 1px ink
/// (#15171c) outline ring, radius 5, centered near the icon's top-right
/// corner at 32x32.
const DOT_COLOR: [u8; 4] = [0xff, 0xd2, 0x3f, 0xff];
const DOT_OUTLINE_COLOR: [u8; 4] = [0x15, 0x17, 0x1c, 0xff];
const DOT_CENTER_X: i32 = 25;
const DOT_CENTER_Y: i32 = 6;
const DOT_RADIUS: i32 = 5;
const DOT_OUTLINE_WIDTH: i32 = 1;
const ICON_SIZE: i32 = 32;

/// Composites the calibration dot onto a copy of a 32x32 RGBA buffer. Pure
/// and allocation-obvious (one `Vec` in, one `Vec` out) so it's cheap to unit
/// test without decoding a real PNG.
fn with_calibration_dot(rgba: &[u8]) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let fill_radius_sq = DOT_RADIUS * DOT_RADIUS;
    let outline_radius = DOT_RADIUS + DOT_OUTLINE_WIDTH;
    let outline_radius_sq = outline_radius * outline_radius;
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x - DOT_CENTER_X;
            let dy = y - DOT_CENTER_Y;
            let dist_sq = dx * dx + dy * dy;
            let color = if dist_sq <= fill_radius_sq {
                Some(DOT_COLOR)
            } else if dist_sq <= outline_radius_sq {
                Some(DOT_OUTLINE_COLOR)
            } else {
                None
            };
            if let Some(color) = color {
                let idx = ((y * ICON_SIZE + x) * 4) as usize;
                out[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }
    out
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

fn tray_icon_from_png_dotted(bytes: &[u8]) -> tray_icon::Icon {
    let (rgba, w, h) = decode_rgba(bytes);
    let dotted = with_calibration_dot(&rgba);
    tray_icon::Icon::from_rgba(dotted, w, h).expect("valid tray icon buffer")
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

    fn blank_rgba() -> Vec<u8> {
        vec![0u8; 32 * 32 * 4]
    }

    fn pixel_at(rgba: &[u8], x: i32, y: i32) -> [u8; 4] {
        let idx = ((y * ICON_SIZE + x) * 4) as usize;
        [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]]
    }

    #[test]
    fn calibration_dot_output_length_matches_input() {
        let input = blank_rgba();
        let out = with_calibration_dot(&input);
        assert_eq!(out.len(), input.len());
    }

    #[test]
    fn calibration_dot_center_pixel_is_the_brand_yellow() {
        let out = with_calibration_dot(&blank_rgba());
        assert_eq!(pixel_at(&out, DOT_CENTER_X, DOT_CENTER_Y), [0xff, 0xd2, 0x3f, 0xff]);
    }

    #[test]
    fn calibration_dot_leaves_pixels_far_from_the_dot_unchanged() {
        let input = blank_rgba();
        let out = with_calibration_dot(&input);
        // Bottom-left corner is nowhere near the top-right dot.
        assert_eq!(pixel_at(&out, 0, 31), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&out, 1, 30), [0, 0, 0, 0]);
    }

    #[test]
    fn calibration_dot_does_not_touch_pixels_outside_its_bounding_box() {
        // A pixel a couple of dot-radii away in every direction stays input.
        let input = blank_rgba();
        let out = with_calibration_dot(&input);
        assert_eq!(pixel_at(&out, DOT_CENTER_X - 10, DOT_CENTER_Y), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&out, DOT_CENTER_X, DOT_CENTER_Y + 10), [0, 0, 0, 0]);
    }
}
