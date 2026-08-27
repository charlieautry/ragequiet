use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::StreamTrait;
use futures::SinkExt;
use iced::widget::column;
use iced::{window, Element, Subscription, Task, Theme};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::alert::AlertGate;
use crate::bridge::{self, Command, CommandTx, SharedLevels};
use crate::config::{CalibrationState, Config};
use crate::detector::{Detector, TrayState};
use crate::engine::Engine;
use crate::ui;
use crate::ui::icons::TrayIcons;
use crate::{alert, audio};

/// Rare events pushed from the audio callback to the UI.
enum AudioEvent {
    State(TrayState),
    Beeped,
}

/// `Subscription::run` only accepts a plain `fn` pointer (no captures), so the
/// receiver built in `boot` is parked here for the subscription's worker thread
/// to claim exactly once.
///
/// This is take-once: safe only as long as `audio_event_subscription()` stays
/// unconditionally present in `subscription()`'s batch, since a second claim
/// attempt (e.g. from a subscription that toggles on/off) would find `None`
/// and silently never receive audio events.
static AUDIO_EVENTS: Mutex<Option<Receiver<AudioEvent>>> = Mutex::new(None);

/// Menu item ids captured at build time; menu events only carry ids.
struct MenuIds {
    enabled: MenuId,
    settings: MenuId,
    quit: MenuId,
}

/// Static markers the meter draws behind the live level, snapshotted from the
/// device's calibration at boot.
pub struct TuningMeta {
    pub noise_floor_db: f32,
    pub quiet_db: Option<f32>,
    pub ceiling_db: Option<f32>,
    /// false = the ceiling is an estimate derived from the baseline margin.
    pub ceiling_confirmed: bool,
    pub sensitivity: f32,
}

pub struct App {
    tray: TrayIcon,
    enabled_item: CheckMenuItem,
    menu_ids: MenuIds,
    settings_window: Option<window::Id>,
    pub(crate) enabled: bool,
    pub(crate) alerts_this_session: u32,
    pub(crate) test_mode: bool,
    pub(crate) config: Config,
    pub(crate) device_name: String,
    shared: Arc<SharedLevels>,
    commands: CommandTx,
    /// None when the input device could not be opened; the app still runs.
    stream: Option<cpal::Stream>,
    pub(crate) tuning_meta: TuningMeta,
    icons: TrayIcons,
    /// (level_db, threshold_db, peak_db) sampled on Tick so `view` stays pure.
    pub(crate) latest: (f32, f32, f32),
    /// Last tray state seen, for the settings window's status line.
    pub(crate) latest_tray_state: TrayState,
    /// True once a `*Changed` message has edited `config` since the last save
    /// (slider release, or a non-drag edit like Ctrl+scroll/arrow keys that
    /// never fires `on_release`); `commit_config` is the only place this is
    /// cleared.
    config_dirty: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    MenuEvent(MenuId),
    TrayStateChanged(TrayState),
    Beeped,
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    Tick,
    SensitivityChanged(f32),
    HoldChanged(u64),
    CooldownChanged(u64),
    TestModeToggled(bool),
    /// Emitted by the chrome row's drag handle (a `mouse_area` over the
    /// title/spacer region); moves the borderless settings window.
    DragWindow,
    /// Emitted by the chrome row's close button.
    CloseSettings,
    /// Emitted on slider release: the single point where a config edit is
    /// persisted to disk (the per-tick `*Changed` messages only push the live
    /// `Command` so the engine reacts immediately, without hammering disk).
    SettingsCommitted,
    Quit,
}

impl App {
    pub fn boot() -> (Self, Task<Message>) {
        let config = Config::load();
        if let Some(path) = Config::path()
            && !path.exists()
        {
            let _ = config.save(); // first run: materialize the file for users to find
        }
        let device_name = audio::default_input_name().unwrap_or_else(|| "unknown input".into());
        let calibration = config.calibration.get(&device_name);
        let tuning = calibration.map(|c| c.tuning()).unwrap_or_default();
        let tuning_meta = TuningMeta {
            noise_floor_db: tuning.noise_floor_db,
            quiet_db: tuning.quiet_db,
            ceiling_db: tuning.ceiling_db,
            ceiling_confirmed: matches!(
                calibration.map(|c| c.state),
                Some(CalibrationState::CeilingSet) | Some(CalibrationState::CeilingLearned)
            ),
            sensitivity: tuning.sensitivity,
        };

        let shared = Arc::new(SharedLevels::default());
        let (commands, command_rx) = bridge::command_channel();
        let (events, event_rx) = std::sync::mpsc::channel::<AudioEvent>();
        if let Ok(mut slot) = AUDIO_EVENTS.lock() {
            *slot = Some(event_rx);
        }

        let mut detector = Detector::new(
            Engine::with_tuning(tuning),
            AlertGate::new(config.hold_ms, config.cooldown_ms),
        );
        let callback_shared = Arc::clone(&shared);
        let start = std::time::Instant::now();
        let stream = audio::start_input(move |frame| {
            while let Ok(cmd) = command_rx.try_recv() {
                detector.apply(cmd);
            }
            let now_ms = start.elapsed().as_millis() as u64;
            let outcome = detector.on_frame(frame, now_ms);
            let threshold_db = detector.threshold_db();
            callback_shared.store(detector.last_level_db(), threshold_db, detector.peak_db());
            if outcome.beep {
                alert::play_beep();
                let _ = events.send(AudioEvent::Beeped);
            }
            if let Some(state) = outcome.state_change {
                let _ = events.send(AudioEvent::State(state));
            }
        })
        .ok();
        if let Some(stream) = &stream {
            let _ = stream.play();
        }

        let enabled_item = CheckMenuItem::new("Enabled", true, true, None);
        let settings_item = MenuItem::new("Settings", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        let menu_ids = MenuIds {
            enabled: enabled_item.id().clone(),
            settings: settings_item.id().clone(),
            quit: quit_item.id().clone(),
        };
        let menu = Menu::new();
        let _ = menu.append_items(&[
            &enabled_item,
            &settings_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ]);

        let icons = TrayIcons::load();
        // No stream means nothing is being monitored: say so with the off icon.
        let initial_icon = if stream.is_some() {
            icons.quiet.clone()
        } else {
            icons.off.clone()
        };
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Ragequiet")
            .with_icon(initial_icon)
            .build()
            .expect("build tray icon");

        (
            Self {
                tray,
                enabled_item,
                menu_ids,
                settings_window: None,
                enabled: true,
                alerts_this_session: 0,
                test_mode: false,
                config,
                device_name,
                shared,
                commands,
                stream,
                tuning_meta,
                icons,
                latest: (-100.0, f32::NAN, -100.0),
                latest_tray_state: TrayState::Quiet,
                config_dirty: false,
            },
            Task::none(), // no window at launch: the app lives in the tray
        )
    }

    pub fn title(&self, _window: window::Id) -> String {
        "Ragequiet".to_string()
    }

    pub fn theme(&self, _window: window::Id) -> Theme {
        crate::ui::theme::theme()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::MenuEvent(id) => {
                if id == self.menu_ids.enabled {
                    self.set_enabled(self.enabled_item.is_checked());
                } else if id == self.menu_ids.settings {
                    return self.open_settings();
                } else if id == self.menu_ids.quit {
                    return Task::done(Message::Quit);
                }
                Task::none()
            }
            Message::TrayStateChanged(state) => {
                self.latest_tray_state = state;
                if self.enabled {
                    let _ = self.tray.set_icon(Some(self.icons.for_state(state)));
                }
                Task::none()
            }
            Message::Beeped => {
                self.alerts_this_session += 1;
                Task::none()
            }
            Message::WindowOpened(id) => {
                self.settings_window = Some(id);
                // First paint uses whatever the audio thread has actually
                // published, instead of the boot-time silence sentinel —
                // avoids a stale reading from before the window existed.
                self.latest = self.shared.load();
                round_corners(id)
            }
            Message::WindowClosed(id) => {
                if self.settings_window == Some(id) {
                    self.settings_window = None;
                }
                self.commit_config();
                Task::none()
            }
            Message::Tick => {
                self.latest = self.shared.load();
                Task::none()
            }
            Message::SensitivityChanged(value) => {
                // Live feel: the engine hears every tick via `SetTuning`, but
                // the config file is only written on `SettingsCommitted`
                // (slider release) so dragging doesn't hammer disk.
                if let Some(entry) = self.config.calibration.get_mut(&self.device_name) {
                    entry.sensitivity = value;
                    let tuning = entry.tuning();
                    let _ = self.commands.send(Command::SetTuning(tuning));
                    self.tuning_meta.sensitivity = tuning.sensitivity;
                    self.config_dirty = true;
                }
                Task::none()
            }
            Message::HoldChanged(hold_ms) => {
                self.config.hold_ms = hold_ms;
                self.config_dirty = true;
                self.send_gate();
                Task::none()
            }
            Message::CooldownChanged(cooldown_ms) => {
                self.config.cooldown_ms = cooldown_ms;
                self.config_dirty = true;
                self.send_gate();
                Task::none()
            }
            Message::TestModeToggled(on) => {
                // Session-only by design: test mode is never persisted.
                self.test_mode = on;
                let _ = self.commands.send(Command::SetTestMode(on));
                Task::none()
            }
            Message::DragWindow => match self.settings_window {
                Some(id) => window::drag(id),
                None => Task::none(),
            },
            Message::CloseSettings => match self.settings_window {
                Some(id) => window::close(id),
                None => Task::none(),
            },
            Message::SettingsCommitted => {
                self.commit_config();
                Task::none()
            }
            Message::Quit => {
                self.commit_config();
                iced::exit()
            }
        }
    }

    pub fn view(&self, window_id: window::Id) -> Element<'_, Message> {
        if Some(window_id) != self.settings_window {
            return column![].into();
        }
        ui::settings::view(self)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            menu_event_subscription(),
            audio_event_subscription(),
            window::close_events().map(Message::WindowClosed),
        ];

        // Sampling (and therefore repainting) only happens while the settings
        // window exists; with no window this is absent from the batch and
        // nothing ever wakes the loop.
        if self.settings_window.is_some() {
            subs.push(iced::time::every(Duration::from_millis(80)).map(|_| Message::Tick));
        }

        Subscription::batch(subs)
    }

    fn open_settings(&mut self) -> Task<Message> {
        if self.settings_window.is_some() {
            return Task::none();
        }
        let (_, open) = window::open(window::Settings {
            size: iced::Size::new(380.0, 560.0),
            resizable: false,
            decorations: false,
            icon: Some(ui::icons::window_icon()),
            ..window::Settings::default()
        });
        open.map(Message::WindowOpened)
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            // Re-announce the state on the next frame and make an in-progress
            // shout re-earn its hold.
            let _ = self.commands.send(Command::SetEnabledIconBaseline);
            if let Some(stream) = &self.stream {
                let _ = stream.play();
                // Only claim "quiet" when a stream actually exists to make it
                // true; with no input device, stay on the off icon rather
                // than lying that monitoring resumed.
                let _ = self.tray.set_icon(Some(self.icons.quiet.clone()));
            } else {
                let _ = self.tray.set_icon(Some(self.icons.off.clone()));
            }
            // Drop any stale state from before the toggle (e.g. Loud lingering
            // across a disable/enable) so the status line doesn't flash a
            // leftover reading it never re-earned.
            self.latest_tray_state = TrayState::Quiet;
        } else {
            if let Some(stream) = &self.stream {
                let _ = stream.pause();
            }
            let _ = self.tray.set_icon(Some(self.icons.off.clone()));
        }
    }

    fn send_gate(&mut self) {
        let _ = self.commands.send(Command::SetGate {
            hold_ms: self.config.hold_ms,
            cooldown_ms: self.config.cooldown_ms,
        });
    }

    /// Whether an input device is actually open and feeding the detector.
    /// `false` means the meter/status line must not present live audio data,
    /// since none exists.
    pub(crate) fn has_stream(&self) -> bool {
        self.stream.is_some()
    }

    /// Writes `config` to disk exactly once per dirty edit, regardless of
    /// which of the three commit points (slider release, window close, quit)
    /// triggers it — covers slider edits made without a drag gesture (Ctrl+
    /// scroll, arrow keys), which iced 0.14 applies live but never fires
    /// `on_release` for.
    fn commit_config(&mut self) {
        if self.config_dirty {
            let _ = self.config.save();
            self.config_dirty = false;
        }
    }
}

/// Threshold implied by the calibration anchors alone — what the engine's
/// `Engine::base_threshold_db` computes from `quiet_db`/`ceiling_db`/
/// `sensitivity`, with no adaptive drift. Used for the meter whenever the
/// audio thread isn't publishing a live threshold (paused, no device, window
/// just opened, or a live threshold that hasn't arrived yet).
pub fn meta_threshold_db(meta: &TuningMeta) -> f32 {
    match (meta.quiet_db, meta.ceiling_db) {
        (Some(q), Some(c)) => q + meta.sensitivity * (c - q),
        (Some(q), None) => q + 4.0 + 6.0 * meta.sensitivity,
        (None, _) => f32::NAN,
    }
}

/// Best-effort Windows 11 rounded corners for the borderless settings window
/// (`DWMWA_WINDOW_CORNER_PREFERENCE` = 33, `DWMWCP_ROUND` = 2). No-op on
/// failure (older Windows, or the window closing before this runs) and on
/// non-Windows targets: square corners are an acceptable fallback.
#[cfg(windows)]
fn round_corners(id: window::Id) -> Task<Message> {
    window::run(id, |handle| {
        use iced::window::raw_window_handle::RawWindowHandle;
        if let Ok(window_handle) = handle.window_handle()
            && let RawWindowHandle::Win32(win32) = window_handle.as_raw()
        {
            const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
            let corner_preference: u32 = 2; // DWMWCP_ROUND
            let hwnd = win32.hwnd.get() as windows_sys::Win32::Foundation::HWND;
            // SAFETY: `hwnd` came from a live raw-window-handle for the
            // window this callback is running against; the attribute value
            // is a plain u32 whose address and size we pass correctly.
            unsafe {
                let _ = windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    (&raw const corner_preference).cast(),
                    std::mem::size_of::<u32>() as u32,
                );
            }
        }
    })
    .discard()
}

#[cfg(not(windows))]
fn round_corners(_id: window::Id) -> Task<Message> {
    Task::none()
}

/// Bridges muda's blocking crossbeam menu-event receiver into iced's async
/// subscription stream: one dedicated OS thread blocks on `recv()` (no polling,
/// so idle CPU stays at zero) and forwards each event into the runtime.
fn menu_event_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            32,
            |mut output: futures::channel::mpsc::Sender<Message>| async move {
                std::thread::spawn(move || {
                    let receiver = MenuEvent::receiver();
                    while let Ok(event) = receiver.recv() {
                        if futures::executor::block_on(
                            output.send(Message::MenuEvent(event.id.clone())),
                        )
                        .is_err()
                        {
                            break;
                        }
                    }
                });

                std::future::pending::<()>().await;
            },
        )
    })
}

/// Same bridge for the audio callback's rare state events (colour changes and
/// fired alerts); the callback itself only does a non-blocking `send`.
fn audio_event_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(
            32,
            |mut output: futures::channel::mpsc::Sender<Message>| async move {
                if let Some(receiver) = AUDIO_EVENTS.lock().ok().and_then(|mut slot| slot.take()) {
                    std::thread::spawn(move || {
                        while let Ok(event) = receiver.recv() {
                            let message = match event {
                                AudioEvent::State(state) => Message::TrayStateChanged(state),
                                AudioEvent::Beeped => Message::Beeped,
                            };
                            if futures::executor::block_on(output.send(message)).is_err() {
                                break;
                            }
                        }
                    });
                }

                std::future::pending::<()>().await;
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(quiet_db: Option<f32>, ceiling_db: Option<f32>, sensitivity: f32) -> TuningMeta {
        TuningMeta { noise_floor_db: -55.0, quiet_db, ceiling_db, ceiling_confirmed: false, sensitivity }
    }

    #[test]
    fn meta_threshold_with_confirmed_ceiling_interpolates_between_anchors() {
        let m = meta(Some(-37.0), Some(-17.0), 0.5);
        assert!((meta_threshold_db(&m) - (-27.0)).abs() < 0.01);
    }

    #[test]
    fn meta_threshold_without_ceiling_uses_the_margin_estimate() {
        let m = meta(Some(-37.0), None, 0.5);
        assert!((meta_threshold_db(&m) - (-30.0)).abs() < 0.01);
    }

    #[test]
    fn meta_threshold_uncalibrated_is_nan() {
        let m = meta(None, None, 0.5);
        assert!(meta_threshold_db(&m).is_nan());
    }
}
