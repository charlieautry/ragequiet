//! The settings window's content: custom chrome (no native title bar) plus
//! the live detection controls. Design direction: Sniffnet/Halloy — dark,
//! single column, generous spacing. The meter itself is a placeholder slot
//! here; Task 4 mounts the real canvas.

use iced::widget::{button, canvas, column, container, mouse_area, row, slider, space, text, toggler};
use iced::{Alignment, Element, Length, Theme};

use crate::app::{banner_visible, drift_nudge_visible, meta_threshold_db, App, Message};
use crate::detector::TrayState;
use crate::ui::meter::Meter;
use crate::ui::theme::{BACKGROUND, FONT_REGULAR, FONT_SEMIBOLD, GREEN, RED, SURFACE, TEXT, TEXT_MUTED, YELLOW};

/// Which reminder banner (if any) should show. Only one shows at a time: the
/// calibration-incomplete banner wins over the drift nudge whenever both
/// would otherwise be visible.
enum Banner {
    /// Calibration incomplete; `urgent` picks the opportunistic daytime copy.
    Incomplete { urgent: bool },
    Drift,
}

fn active_banner(app: &App) -> Option<Banner> {
    if banner_visible(app.calibration_incomplete, app.config.banner_dismissed_on.as_deref(), &app.today) {
        let urgent = app.alerts_this_session > 0 && (7..=21).contains(&app.hour);
        Some(Banner::Incomplete { urgent })
    } else if drift_nudge_visible(app.calibration_incomplete, app.latest.3, app.drift_nudge_dismissed) {
        Some(Banner::Drift)
    } else {
        None
    }
}

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
    // The wizard takes over the whole body while it runs (the chrome row
    // stays, so the window is still movable/closable); the live meter is
    // handed to it so users watch their own level while calibrating.
    let body: Element<'_, Message> = match &app.wizard {
        Some(wizard) => crate::ui::calibrate::view(wizard, meter_panel(app)),
        None => {
            let mut col = column![].spacing(16).width(Length::Fill);
            if let Some(banner) = banner_row(app) {
                col = col.push(banner);
            }
            col.push(meter_panel(app))
                .push(status_line(app))
                .push(alerts_line(app))
                .push(sensitivity_block(app))
                .push(hold_block(app))
                .push(cooldown_block(app))
                .push(test_mode_row(app))
                .push(input_line(app))
                .into()
        }
    };

    let content = column![chrome_row(), body]
        .spacing(16)
        .width(Length::Fill)
        .height(Length::Fill);

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

/// A one-line dismissible reminder at the top of the controls view: never
/// shown while the wizard is running, and never more than one at a time (the
/// calibration-incomplete banner wins over the drift nudge — see
/// `active_banner`).
fn banner_row(app: &App) -> Option<Element<'_, Message>> {
    let (message, action_label, action, dismiss): (&str, &str, Message, Message) = match active_banner(app)? {
        Banner::Incomplete { urgent: true } => (
            "Sounds like you're free to be loud right now. Finish calibration? (5 seconds)",
            "Calibrate",
            Message::WizardStarted,
            Message::BannerDismissed,
        ),
        Banner::Incomplete { urgent: false } => (
            "Calibration incomplete. Finish the loud step for the most accurate detection.",
            "Calibrate",
            Message::WizardStarted,
            Message::BannerDismissed,
        ),
        Banner::Drift => (
            "Your mic level has shifted since calibration — recalibrate? (takes ~15 s)",
            "Recalibrate",
            Message::WizardStarted,
            Message::DriftNudgeDismissed,
        ),
    };

    let content = row![
        text(message).font(FONT_REGULAR).size(12).color(TEXT).width(Length::Fill),
        button(text(action_label).font(FONT_SEMIBOLD).size(12))
            .padding([4.0, 10.0])
            .style(banner_action_style)
            .on_press(action),
        button(text("✕").size(12)).padding([2.0, 8.0]).style(close_button_style).on_press(dismiss),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    Some(
        container(content)
            .width(Length::Fill)
            .padding(10)
            .style(|_theme: &Theme| container::Style {
                background: Some(SURFACE.into()),
                border: iced::border::rounded(8.0),
                ..container::Style::default()
            })
            .into(),
    )
}

fn banner_action_style(_theme: &Theme, status: button::Status) -> button::Style {
    let mut background = GREEN;
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        background.a = 0.85;
    }
    button::Style {
        background: Some(background.into()),
        text_color: BACKGROUND,
        border: iced::border::rounded(6.0),
        ..button::Style::default()
    }
}

fn close_button_style(_theme: &Theme, status: button::Status) -> button::Style {
    let text_color = if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        RED
    } else {
        TEXT_MUTED
    };
    button::Style { text_color, ..button::Style::default() }
}

/// The live level meter: current level filled from the left in the state
/// color, calibration markers (noise floor / quiet point / threshold /
/// ceiling) drawn behind it, red-shaded region past the threshold. Paused
/// (`!app.enabled`) shows an empty bar with the markers still visible, since
/// they're a property of the calibration, not the live signal.
fn meter_panel(app: &App) -> Element<'_, Message> {
    let (raw_level_db, live_threshold_db, raw_peak_db, _drift_db) = app.latest;
    let live = app.enabled && app.has_stream();
    let (level_db, peak_db) = if live { (raw_level_db, raw_peak_db) } else { (-100.0, -100.0) };
    // The audio thread's threshold is only trustworthy while it's actually
    // live and has published a finite reading; otherwise fall back to what
    // the calibration anchors alone imply, so the meter never shows a stale
    // or default (0 dBFS -> permanent red) threshold.
    let threshold_db = if live && live_threshold_db.is_finite() {
        live_threshold_db
    } else {
        meta_threshold_db(&app.tuning_meta)
    };

    canvas(Meter {
        level_db,
        threshold_db,
        peak_db,
        noise_floor_db: app.tuning_meta.noise_floor_db,
        quiet_db: app.tuning_meta.quiet_db,
        ceiling_db: app.tuning_meta.ceiling_db,
        ceiling_confirmed: app.tuning_meta.ceiling_confirmed,
    })
    .width(Length::Fill)
    .height(Length::Fixed(48.0))
    .into()
}

fn status_line(app: &App) -> Element<'_, Message> {
    // No device outranks Paused/state colors: it's true regardless of the
    // enabled toggle, and a colored state label over an empty meter would
    // otherwise claim a reading that doesn't exist.
    let (label, color) = if !app.has_stream() {
        ("No microphone", TEXT_MUTED)
    } else if !app.enabled {
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

/// Sensitivity slider: disabled (a static bar, no interaction) whenever the
/// engine ignores sensitivity — which follows `tuning_meta.quiet_db`, not
/// merely whether a calibration entry exists, since a half-parsed entry
/// (e.g. a NaN quiet point) still falls back to uncalibrated `Tuning` even
/// though `config.calibration` has an entry for the device.
fn sensitivity_block(app: &App) -> Element<'_, Message> {
    let has_calibration = app.tuning_meta.quiet_db.is_some();
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
