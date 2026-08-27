//! The settings window's content: custom chrome (no native title bar) plus
//! the live detection controls. Design direction: Sniffnet/Halloy — dark,
//! single column, generous spacing. The meter itself is a placeholder slot
//! here; Task 4 mounts the real canvas.

use iced::widget::{button, column, container, mouse_area, row, slider, space, text, toggler};
use iced::{Alignment, Element, Length, Theme};

use crate::app::{App, Message};
use crate::detector::TrayState;
use crate::ui::theme::{BACKGROUND, FONT_REGULAR, FONT_SEMIBOLD, GREEN, RED, SURFACE, TEXT, TEXT_MUTED, YELLOW};

/// Seconds shown/edited by the cooldown slider; the config field stays in ms.
pub fn cooldown_ms_to_s(cooldown_ms: u64) -> u64 {
    (cooldown_ms / 1000).clamp(1, 10)
}

/// Inverse of [`cooldown_ms_to_s`]; carries whole seconds back into ms for
/// `Message::CooldownChanged`, which the rest of the app persists in ms.
pub fn cooldown_s_to_ms(seconds: u64) -> u64 {
    seconds.clamp(1, 10) * 1000
}

pub fn view(app: &App) -> Element<'_, Message> {
    let content = column![
        chrome_row(),
        meter_placeholder(),
        status_line(app),
        alerts_line(app),
        sensitivity_block(app),
        hold_block(app),
        cooldown_block(app),
        test_mode_row(app),
        input_line(app),
    ]
    .spacing(16)
    .width(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(24)
        .style(|_theme: &Theme| container::Style {
            background: Some(BACKGROUND.into()),
            ..container::Style::default()
        })
        .into()
}

/// The custom title bar: draggable title region on the left, flat close
/// button on the right. Borderless windows (`decorations: false`) have no
/// native chrome, so this row is the only way to move or close the window.
fn chrome_row<'a>() -> Element<'a, Message> {
    let title = text("Ragequiet").font(FONT_SEMIBOLD).size(14).color(TEXT);

    // Only the title + spacer is draggable; the close button must stay
    // clickable rather than starting a window drag.
    let drag_region = mouse_area(row![title, space::horizontal()].align_y(Alignment::Center).width(Length::Fill))
        .on_press(Message::DragWindow);

    let close = button(text("✕").size(14))
        .padding([2.0, 8.0])
        .style(close_button_style)
        .on_press(Message::CloseSettings);

    row![drag_region, close]
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fixed(36.0))
        .into()
}

fn close_button_style(_theme: &Theme, status: button::Status) -> button::Style {
    let text_color = if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        RED
    } else {
        TEXT_MUTED
    };
    button::Style { text_color, ..button::Style::default() }
}

/// A fixed-size slot where the meter canvas will live (Task 4); for now just
/// a labeled panel so the layout reads correctly.
fn meter_placeholder<'a>() -> Element<'a, Message> {
    container(text("meter").font(FONT_REGULAR).size(12).color(TEXT_MUTED))
        .center_x(Length::Fill)
        .center_y(Length::Fixed(48.0))
        .style(|_theme: &Theme| container::Style {
            background: Some(SURFACE.into()),
            border: iced::border::rounded(8.0),
            ..container::Style::default()
        })
        .into()
}

fn status_line(app: &App) -> Element<'_, Message> {
    let (label, color) = if !app.enabled {
        ("Paused", TEXT_MUTED)
    } else {
        match app.latest_tray_state {
            TrayState::Quiet => ("Quiet", GREEN),
            TrayState::Warning => ("Getting loud", YELLOW),
            TrayState::Loud => ("Too loud", RED),
        }
    };
    text(label).font(FONT_SEMIBOLD).size(20).color(color).into()
}

fn alerts_line(app: &App) -> Element<'_, Message> {
    text(format!("Alerts this session: {}", app.alerts_this_session))
        .font(FONT_REGULAR)
        .size(13)
        .color(TEXT_MUTED)
        .into()
}

/// Sensitivity slider: disabled (a static bar, no interaction) when the
/// device has no calibration entry at all, since an uncalibrated engine
/// ignores sensitivity entirely.
fn sensitivity_block(app: &App) -> Element<'_, Message> {
    let has_calibration = app.config.calibration.contains_key(&app.device_name);
    let label = if app.tuning_meta.ceiling_db.is_none() {
        "Sensitivity (estimated)"
    } else {
        "Sensitivity"
    };
    let value = app.tuning_meta.sensitivity;

    let header = row![
        text(label).font(FONT_REGULAR).size(13).color(TEXT),
        space::horizontal(),
        text(format!("{:.0}%", value * 100.0)).font(FONT_REGULAR).size(13).color(TEXT_MUTED),
    ];

    let mut block = column![header].spacing(6);

    if has_calibration {
        let control = slider(0.0..=1.0, value, Message::SensitivityChanged)
            .step(0.01)
            .on_release(Message::SettingsCommitted);
        block = block.push(control);
    } else {
        // Non-interactive stand-in: iced's slider always requires an
        // on_change closure, so a genuinely disabled slider is rendered as a
        // static two-segment bar (filled/empty portions) rather than a
        // slider with a no-op handler.
        let filled_portion = (value.clamp(0.0, 1.0) * 100.0).round().clamp(1.0, 99.0) as u16;
        let empty_portion = 100 - filled_portion;
        let filled = container(text(""))
            .width(Length::FillPortion(filled_portion))
            .height(Length::Fill)
            .style(|_theme: &Theme| container::Style {
                background: Some(TEXT_MUTED.into()),
                ..container::Style::default()
            });
        let empty = container(text("")).width(Length::FillPortion(empty_portion)).height(Length::Fill);
        let bar = container(row![filled, empty].width(Length::Fill).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fixed(6.0))
            .style(|_theme: &Theme| container::Style {
                background: Some(SURFACE.into()),
                border: iced::border::rounded(4.0),
                ..container::Style::default()
            });
        let hint = text("calibrate to enable").font(FONT_REGULAR).size(12).color(TEXT_MUTED);
        block = block.push(bar).push(hint);
    }

    block.into()
}

fn hold_block(app: &App) -> Element<'_, Message> {
    let hold_ms = app.config.hold_ms;
    let header = row![
        text("Hold time").font(FONT_REGULAR).size(13).color(TEXT),
        space::horizontal(),
        text(format!("{hold_ms} ms")).font(FONT_REGULAR).size(13).color(TEXT_MUTED),
    ];
    // iced's slider requires `T: Into<f64>`, which `u64` doesn't implement
    // (lossy in general); the config/message value stays u64, the slider's
    // own numeric type is f32.
    let control = slider(100.0..=1000.0f32, hold_ms as f32, |v| Message::HoldChanged(v.round() as u64))
        .step(50.0f32)
        .on_release(Message::SettingsCommitted);
    column![header, control].spacing(6).into()
}

fn cooldown_block(app: &App) -> Element<'_, Message> {
    let cooldown_s = cooldown_ms_to_s(app.config.cooldown_ms);
    let header = row![
        text("Cooldown").font(FONT_REGULAR).size(13).color(TEXT),
        space::horizontal(),
        text(format!("{cooldown_s} s")).font(FONT_REGULAR).size(13).color(TEXT_MUTED),
    ];
    let control = slider(1.0..=10.0f32, cooldown_s as f32, |v| {
        Message::CooldownChanged(cooldown_s_to_ms(v.round() as u64))
    })
    .step(1.0f32)
    .on_release(Message::SettingsCommitted);
    column![header, control].spacing(6).into()
}

fn test_mode_row(app: &App) -> Element<'_, Message> {
    toggler(app.test_mode)
        .label("Test mode (no sound)")
        .on_toggle(Message::TestModeToggled)
        .font(FONT_REGULAR)
        .into()
}

fn input_line(app: &App) -> Element<'_, Message> {
    text(format!("Input: {}", app.device_name))
        .font(FONT_REGULAR)
        .size(12)
        .color(TEXT_MUTED)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_seconds_round_trip_on_thousand_boundaries() {
        for s in 1..=10u64 {
            assert_eq!(cooldown_ms_to_s(cooldown_s_to_ms(s)), s);
        }
    }

    #[test]
    fn cooldown_ms_clamps_into_the_slider_range() {
        assert_eq!(cooldown_ms_to_s(0), 1, "below range clamps to 1 s");
        assert_eq!(cooldown_ms_to_s(500), 1, "sub-second values round down but still clamp to 1 s");
        assert_eq!(cooldown_ms_to_s(60_000), 10, "above range clamps to 10 s");
    }

    #[test]
    fn cooldown_seconds_clamp_into_ms_range() {
        assert_eq!(cooldown_s_to_ms(0), 1000, "below range clamps to 1 s");
        assert_eq!(cooldown_s_to_ms(99), 10_000, "above range clamps to 10 s");
    }
}
