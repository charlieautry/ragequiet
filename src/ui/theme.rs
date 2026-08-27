//! Dark palette, accent colors, and the embedded Inter font handles.
//!
//! Design direction: dark by default, one accent trio matching the tray
//! state colors (green/yellow/red), modeled on Sniffnet/Halloy rather than
//! iced's stock demo look.

use iced::theme::Palette;
use iced::{Color, Font, Theme};

// Brand palette (assets/brand/README.md): green #3ed67a, amber #ffb020,
// red #ff3b4a, ink #15171c.
pub const GREEN: Color = Color::from_rgb8(0x3e, 0xd6, 0x7a);
pub const YELLOW: Color = Color::from_rgb8(0xff, 0xb0, 0x20);
pub const RED: Color = Color::from_rgb8(0xff, 0x3b, 0x4a);

pub const BACKGROUND: Color = Color::from_rgb8(0x15, 0x17, 0x1c);
/// Slightly lighter panel color for the meter background and other
/// panel-like surfaces (the meter placeholder slot).
pub const SURFACE: Color = Color::from_rgb8(0x1d, 0x20, 0x26);
pub const TEXT: Color = Color::from_rgb(0.92, 0.92, 0.93);
pub const TEXT_MUTED: Color = Color::from_rgb(0.55, 0.55, 0.58);

pub const FONT_REGULAR: Font = Font::with_name("Inter");
pub const FONT_SEMIBOLD: Font = Font {
    weight: iced::font::Weight::Semibold,
    ..Font::with_name("Inter")
};

pub const FONT_REGULAR_BYTES: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
pub const FONT_SEMIBOLD_BYTES: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

/// The app's custom dark theme: iced 0.14's `Palette` has six required
/// fields (background, text, primary, success, warning, danger) — no
/// `surface` field exists on `Palette` itself (that's derived into the
/// `Extended` palette iced generates from these six).
pub fn theme() -> Theme {
    Theme::custom(
        "ragequiet".to_string(),
        Palette {
            background: BACKGROUND,
            text: TEXT,
            primary: GREEN,
            success: GREEN,
            warning: YELLOW,
            danger: RED,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_fonts_are_real_ttfs() {
        // Sanity check the embedded bytes look like TrueType data (magic
        // number 0x00010000) and are the static, non-variable files
        // (roughly a few hundred KB, not the multi-MB variable font).
        assert_eq!(&FONT_REGULAR_BYTES[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(&FONT_SEMIBOLD_BYTES[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert!(FONT_REGULAR_BYTES.len() < 1_000_000);
        assert!(FONT_SEMIBOLD_BYTES.len() < 1_000_000);
    }

    #[test]
    fn theme_uses_green_primary() {
        assert_eq!(theme().palette().primary, GREEN);
    }
}
