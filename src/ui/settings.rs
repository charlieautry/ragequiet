//! The settings window's content: custom chrome (no native title bar) plus
//! the live detection controls. Design direction: Sniffnet/Halloy — dark,
//! single column, generous spacing. The meter itself is a placeholder slot
//! here; Task 4 mounts the real canvas.

use iced::widget::{
    button, canvas, column, container, mouse_area, pick_list, row, scrollable, slider, space, stack, text, toggler,
};
use iced::window::Direction as ResizeDirection;
use iced::{Alignment, Element, Length, Theme, mouse};

use crate::app::{banner_visible, drift_nudge_visible, meta_threshold_db, App, Message, SoundChoice};
use crate::detector::TrayState;
use crate::sounds;
use crate::ui::meter::Meter;
use crate::ui::theme::{BACKGROUND, FONT_REGULAR, FONT_SEMIBOLD, GREEN, RED, SURFACE, TEXT, TEXT_MUTED, YELLOW};

/// "System default" is the picker's sentinel text for `output_device: None`;
/// no real cpal device is expected to collide with this label.
const SYSTEM_DEFAULT_DEVICE: &str = "System default";

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
        Some(wizard) => crate::ui::calibrate::view(
            wizard,
            meter_panel(app),
            !app.enabled || !app.has_stream(),
            app.wizard_stall_hint(),
            app.device_changed_note,
        ),
        None => {
            let mut col = column![].spacing(12).width(Length::Fill);
            if let Some(error) = &app.wizard_error {
                col = col.push(wizard_error_row(error));
            }
            if let Some(banner) = banner_row(app) {
                col = col.push(banner);
            }
            col.push(meter_panel(app))
                .push(status_line(app))
                .push(alerts_line(app))
                .push(sensitivity_block(app))
                .push(hold_block(app))
                .push(cooldown_block(app))
                .push(alert_sound_block(app))
                .push(alert_volume_block(app))
                .push(output_device_block(app))
                .push(test_mode_row(app))
                .push(autostart_block(app))
                .push(input_line(app))
                .push(cpu_line(app))
                .into()
        }
    };

    // The default controls stack (no banner, no wizard error) fits inside the
    // 400x760 window on its own — meter/status/alerts/sensitivity/hold/
    // cooldown/sound/volume/output-device/test-mode/autostart/input/CPU is 13
    // rows at 12px spacing, comfortably under the ~680px available once the
    // chrome bar and body padding are subtracted. This scrollable is a safety
    // net, not the primary layout: if a banner, wrapped alert copy, or a long
    // device name ever pushes the body past the fold anyway, every control
    // (including Cancel/Finish) still stays reachable instead of clipping.
    // The chrome row lives outside this scrollable so the window stays
    // movable/closable no matter how tall the body gets.
    let scrollable_body = scrollable(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new().width(4.0).margin(2.0).scroller_width(4.0),
        ))
        .style(scrollbar_style);

    // The body's own padding: tighter than the old uniform 24px on every
    // side (14 horizontal, 12 on top — the bottom edge gets a touch more,
    // 14, to balance the scrollbar's margin). The chrome bar below is
    // full-bleed and carries none of this, so it isn't inset from the
    // window edges the way the body is.
    let body_container = container(scrollable_body)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding { top: 12.0, right: 14.0, bottom: 14.0, left: 14.0 });

    // No spacing here: the chrome bar spans edge-to-edge and the body's own
    // top padding (above) is what separates it from the bar, so the bar's
    // padding is the only "gap" above the title and it's inside the drag
    // mouse_area, not dead space outside it.
    let content = column![chrome_row(), body_container].width(Length::Fill).height(Length::Fill);

    // No padding here either — the chrome bar must reach every window edge
    // for the drag mouse_area (which fills the bar) to be full-bleed. The
    // body's own padding above provides all of the interior breathing room.
    let window_body = container(content).width(Length::Fill).height(Length::Fill).style(
        |_theme: &Theme| container::Style { background: Some(BACKGROUND.into()), ..container::Style::default() },
    );

    // Invisible edge/corner strips layered over everything so the window can
    // still be resized (see `resize_handles`'s doc comment for why the OS's
    // own edge hit-testing doesn't do this for us).
    stack![window_body, resize_handles()].into()
}

/// Edge and corner drag-resize strips for the borderless settings window.
///
/// A decorated (native-chrome) window gets ~8px invisible resize borders for
/// free from Windows' own `WM_NCHITTEST` handling. This window turns
/// decorations off, and winit implements that by having `WM_NCCALCSIZE`
/// claim the *entire* window rect as client area (see winit 0.30's
/// `platform_impl::windows::event_loop`, the `WM_NCCALCSIZE` arm: it returns
/// `Value(0)` — "no non-client area" — whenever `MARKER_DECORATIONS` is
/// unset). Verified empirically too: probing `WM_NCHITTEST` one pixel in
/// from the live window's right edge returns `HTCLIENT` (1), never `HTRIGHT`
/// (11), even with `resizable: true` and `WS_SIZEBOX` set on the HWND — so
/// there is no native resize border left to rely on here.
///
/// The fallback is the same trick the chrome row's drag handle already uses
/// for moving the window: `iced::window::drag_resize` hands off to the OS's
/// modal resize loop directly (winit's `drag_resize_window`, which posts a
/// synthetic `WM_NCLBUTTONDOWN` with the matching `HT*` code) rather than
/// depending on hit-testing at all. Each strip below is a thin `mouse_area`
/// pinned to one edge or corner via `container`'s alignment, stacked over
/// the whole window; only its own narrow band intercepts the cursor; every
/// pixel outside the bands reaches the normal UI underneath untouched.
fn resize_handles<'a>() -> Element<'a, Message> {
    const EDGE: f32 = 6.0;
    const CORNER: f32 = 10.0;

    // A thin `mouse_area` of the given size, pinned to one edge/corner of
    // the whole window via `container`'s alignment (the container itself is
    // Fill-sized so the alignment has room to push the strip to the edge;
    // only the strip's own small bounds intercept the cursor).
    let handle = |width: Length,
                  height: Length,
                  align_x: Alignment,
                  align_y: Alignment,
                  interaction: mouse::Interaction,
                  direction: ResizeDirection| {
        let strip = mouse_area(space::Space::new().width(width).height(height))
            .interaction(interaction)
            .on_press(Message::ResizeWindow(direction));

        container(strip).width(Length::Fill).height(Length::Fill).align_x(align_x).align_y(align_y)
    };

    use mouse::Interaction::{ResizingDiagonallyDown, ResizingDiagonallyUp, ResizingHorizontally, ResizingVertically};
    use ResizeDirection::{East, North, NorthEast, NorthWest, South, SouthEast, SouthWest, West};

    let edge = Length::Fixed(EDGE);
    let corner = Length::Fixed(CORNER);

    stack![
        // Edges: full-length strips along each side.
        handle(edge, Length::Fill, Alignment::End, Alignment::Center, ResizingHorizontally, East),
        handle(edge, Length::Fill, Alignment::Start, Alignment::Center, ResizingHorizontally, West),
        handle(Length::Fill, edge, Alignment::Center, Alignment::Start, ResizingVertically, North),
        handle(Length::Fill, edge, Alignment::Center, Alignment::End, ResizingVertically, South),
        // Corners: small squares layered on top of (and overriding) the edge
        // strips right at the corners, one per diagonal.
        handle(corner, corner, Alignment::Start, Alignment::Start, ResizingDiagonallyDown, NorthWest),
        handle(corner, corner, Alignment::End, Alignment::Start, ResizingDiagonallyUp, NorthEast),
        handle(corner, corner, Alignment::Start, Alignment::End, ResizingDiagonallyUp, SouthWest),
        handle(corner, corner, Alignment::End, Alignment::End, ResizingDiagonallyDown, SouthEast),
    ]
    .into()
}

/// The custom title bar: draggable title region on the left, flat close
/// button on the right. Borderless windows (`decorations: false`) have no
/// native chrome, so this row is the only way to move or close the window.
///
/// This bar is full-bleed (no outer padding of its own — see `view`), and
/// the padding that visually surrounds the title lives *inside* the drag
/// `mouse_area` rather than around it. That's deliberate: previously the
/// window's outer container padding sat around this row too, so the space
/// above/left of the title looked draggable but wasn't. Now every pixel of
/// the top band — except the ✕ button and the 6px resize strips the stack
/// overlays on the very edges (see `resize_handles`) — is inside the
/// mouse_area and drags the window.
fn chrome_row<'a>() -> Element<'a, Message> {
    let title = text("Ragequiet").font(FONT_SEMIBOLD).size(14).color(TEXT);

    // The padding here (10 top/bottom, 14 left, 0 right so the spacer runs
    // flush to the close button) is inside the mouse_area, not outside it.
    let drag_region = mouse_area(
        container(row![title, space::horizontal()].align_y(Alignment::Center))
            .width(Length::Fill)
            .padding(iced::Padding { top: 10.0, right: 0.0, bottom: 10.0, left: 14.0 }),
    )
    .on_press(Message::DragWindow);

    // The close button carries its own (larger) padding so its hit target
    // stays comfortable even though the bar around it lost its outer inset.
    let close = button(text("✕").size(14))
        .padding([10.0, 14.0])
        .style(close_button_style)
        .on_press(Message::CloseSettings);

    row![drag_region, close].align_y(Alignment::Center).width(Length::Fill).into()
}

/// Inline notice shown when the tray "Recalibrate"/banner Calibrate buttons
/// were pressed with no input device open (see `Message::WizardStarted`'s
/// gating) — the same muted-error idiom as `sound_error`/`autostart_error`,
/// just placed up top near the banner since there's no specific control it
/// hangs off of.
fn wizard_error_row(message: &str) -> Element<'_, Message> {
    text(message.to_string()).font(FONT_REGULAR).size(12).color(TEXT_MUTED).width(Length::Fill).into()
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

    // The message sits alone on its own row so it always gets the banner's
    // full width to wrap into (never starved by the buttons' widths); the
    // actions sit right-aligned on their own row below it.
    let content = column![
        text(message).font(FONT_REGULAR).size(12).color(TEXT).width(Length::Fill),
        row![
            space::horizontal(),
            button(text(action_label).font(FONT_SEMIBOLD).size(12))
                .padding([4.0, 10.0])
                .style(banner_action_style)
                .on_press(action),
            button(text("✕").size(12)).padding([2.0, 8.0]).style(close_button_style).on_press(dismiss),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(8);

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

/// Thin, unobtrusive scrollbar for the body's scrollable safety net: a
/// surface-colored rail rather than the theme's default (primary-colored on
/// hover), so it reads as incidental rather than a primary control.
fn scrollbar_style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut style = scrollable::default(theme, status);
    let rail = scrollable::Rail {
        background: None,
        border: iced::border::rounded(2.0),
        scroller: scrollable::Scroller {
            background: SURFACE.into(),
            border: iced::border::rounded(2.0),
        },
    };
    style.vertical_rail = rail;
    style.horizontal_rail = rail;
    style
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
    text(label).font(FONT_SEMIBOLD).size(20).color(color).width(Length::Fill).into()
}

fn alerts_line(app: &App) -> Element<'_, Message> {
    text(format!("Alerts this session: {}", app.alerts_this_session))
        .font(FONT_REGULAR)
        .size(13)
        .color(TEXT_MUTED)
        .width(Length::Fill)
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

/// "Alert sound" picker: the twelve built-ins plus a "Custom…" entry that opens
/// the file dialog; a chosen custom file's name becomes the closed control's
/// display text (see `SoundChoice`/`App::sound_choice`). A **Test** button
/// alongside it plays the current selection through the configured device.
/// Two independent inline errors can show below it: `sound_error` (the
/// configured file failed to decode) and `output_error` (the output device
/// failed to open — polled from the player worker; see `App::output_error`).
fn alert_sound_block(app: &App) -> Element<'_, Message> {
    let options: Vec<SoundChoice> = sounds::ALL
        .iter()
        .map(|&b| SoundChoice::Builtin(b))
        .chain(std::iter::once(SoundChoice::Custom(None)))
        .collect();

    let picker = pick_list(options, Some(app.sound_choice()), Message::AlertSoundPicked)
        .font(FONT_REGULAR)
        .text_size(13)
        .width(Length::Fill);

    let test_button = button(text("Test").font(FONT_REGULAR).size(13))
        .padding([4.0, 12.0])
        .style(banner_action_style)
        .on_press(Message::TestSound);

    let mut block = column![
        text("Alert sound").font(FONT_REGULAR).size(13).color(TEXT),
        row![picker, test_button].spacing(8).align_y(Alignment::Center),
    ]
    .spacing(6);

    if let Some(error) = &app.sound_error {
        block = block.push(text(error).font(FONT_REGULAR).size(12).color(RED));
    }

    if let Some(error) = &app.output_error {
        block = block.push(text(error).font(FONT_REGULAR).size(12).color(RED));
    }

    block.into()
}

fn alert_volume_block(app: &App) -> Element<'_, Message> {
    let value = app.config.effective_volume();
    let header = row![
        text("Alert volume").font(FONT_REGULAR).size(13).color(TEXT),
        space::horizontal(),
        text(format!("{:.0}%", value * 100.0)).font(FONT_REGULAR).size(13).color(TEXT_MUTED),
    ];
    let control = slider(0.0..=1.0, value, Message::AlertVolumeChanged)
        .step(0.01)
        .on_release(Message::SettingsCommitted);
    column![header, control].spacing(6).into()
}

/// "Output device" picker: "System default" (maps to `output_device: None`)
/// plus every device `Message::WindowOpened` enumerated. Falls back to just
/// the default entry if enumeration ever comes back empty (no devices, or
/// the window hasn't opened yet), so the control is never empty.
fn output_device_block(app: &App) -> Element<'_, Message> {
    let mut options = vec![SYSTEM_DEFAULT_DEVICE.to_string()];
    options.extend(app.output_devices.iter().cloned());

    let selected = app.config.output_device.clone().unwrap_or_else(|| SYSTEM_DEFAULT_DEVICE.to_string());

    let picker = pick_list(options, Some(selected), |choice: String| {
        Message::OutputDevicePicked(if choice == SYSTEM_DEFAULT_DEVICE { None } else { Some(choice) })
    })
    .font(FONT_REGULAR)
    .text_size(13)
    .width(Length::Fill);

    column![text("Output device").font(FONT_REGULAR).size(13).color(TEXT), picker].spacing(6).into()
}

fn test_mode_row(app: &App) -> Element<'_, Message> {
    toggler(app.test_mode)
        .label("Test mode (no sound)")
        .on_toggle(Message::TestModeToggled)
        .font(FONT_REGULAR)
        .into()
}

/// "Start with Windows" toggler: state comes from `app.autostart`, refreshed
/// from the registry on every `WindowOpened` (see `App::boot`/`WindowOpened`
/// handler) rather than from `config.start_with_windows`, since the registry
/// is the runtime source of truth. A failed toggle shows an inline muted
/// error line instead of touching the checkbox.
fn autostart_block(app: &App) -> Element<'_, Message> {
    let mut block = column![
        toggler(app.autostart).label("Start with Windows").on_toggle(Message::AutostartToggled).font(FONT_REGULAR),
    ]
    .spacing(6);

    if let Some(error) = &app.autostart_error {
        block = block.push(text(error).font(FONT_REGULAR).size(12).color(RED));
    }

    block.into()
}

fn input_line(app: &App) -> Element<'_, Message> {
    // The device name is arbitrary, driver-supplied text (easily 60+ chars);
    // an explicit fill width lets it wrap across lines instead of clipping.
    text(format!("Input: {}", app.device_name))
        .font(FONT_REGULAR)
        .size(12)
        .color(TEXT_MUTED)
        .width(Length::Fill)
        .into()
}

/// This process's own CPU usage (spec §7), smoothed on `Tick` — see
/// `App::cpu_percent_smoothed`. Reads 0.0 on non-Windows targets, where
/// `sysinfo::process_cpu_100ns` never returns a sample.
fn cpu_line(app: &App) -> Element<'_, Message> {
    text(format!("CPU: {:.1}%", app.cpu_percent_smoothed))
        .font(FONT_REGULAR)
        .size(12)
        .color(TEXT_MUTED)
        .width(Length::Fill)
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
