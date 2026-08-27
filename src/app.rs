use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::StreamTrait;
use futures::SinkExt;
use iced::widget::{column, container, text};
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
static AUDIO_EVENTS: Mutex<Option<Receiver<AudioEvent>>> = Mutex::new(None);

/// Menu item ids captured at build time; menu events only carry ids.
struct MenuIds {
    enabled: MenuId,
    settings: MenuId,
    quit: MenuId,
}

/// Static markers the meter draws behind the live level, snapshotted from the
/// device's calibration at boot.
#[expect(dead_code)] // consumed by the settings view + meter in a later task
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
    enabled: bool,
    alerts_this_session: u32,
    test_mode: bool,
    config: Config,
    device_name: String,
    shared: Arc<SharedLevels>,
    commands: CommandTx,
    /// None when the input device could not be opened; the app still runs.
    stream: Option<cpal::Stream>,
    tuning_meta: TuningMeta,
    icons: TrayIcons,
    /// (level_db, threshold_db, peak_db) sampled on Tick so `view` stays pure.
    latest: (f32, f32, f32),
}

#[derive(Debug, Clone)]
pub enum Message {
    MenuEvent(MenuId),
    TrayStateChanged(TrayState),
    Beeped,
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    Tick,
    #[expect(dead_code)] // emitted by the settings view in a later task
    SensitivityChanged(f32),
    #[expect(dead_code)]
    HoldChanged(u64),
    #[expect(dead_code)]
    CooldownChanged(u64),
    #[expect(dead_code)]
    TestModeToggled(bool),
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
                latest: (0.0, 0.0, 0.0),
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
                Task::none()
            }
            Message::WindowClosed(id) => {
                if self.settings_window == Some(id) {
                    self.settings_window = None;
                }
                Task::none()
            }
            Message::Tick => {
                self.latest = self.shared.load();
                Task::none()
            }
            Message::SensitivityChanged(value) => {
                if let Some(entry) = self.config.calibration.get_mut(&self.device_name) {
                    entry.sensitivity = value;
                    let tuning = entry.tuning();
                    let _ = self.commands.send(Command::SetTuning(tuning));
                    self.tuning_meta.sensitivity = tuning.sensitivity;
                    let _ = self.config.save();
                }
                Task::none()
            }
            Message::HoldChanged(hold_ms) => {
                self.config.hold_ms = hold_ms;
                self.send_gate();
                Task::none()
            }
            Message::CooldownChanged(cooldown_ms) => {
                self.config.cooldown_ms = cooldown_ms;
                self.send_gate();
                Task::none()
            }
            Message::TestModeToggled(on) => {
                self.test_mode = on;
                let _ = self.commands.send(Command::SetTestMode(on));
                Task::none()
            }
            Message::Quit => iced::exit(),
        }
    }

    pub fn view(&self, window_id: window::Id) -> Element<'_, Message> {
        if Some(window_id) != self.settings_window {
            return column![].into();
        }
        let title = text("Ragequiet")
            .font(ui::theme::FONT_SEMIBOLD)
            .size(24)
            .color(ui::theme::TEXT);
        let subtitle = text("settings window")
            .font(ui::theme::FONT_REGULAR)
            .size(14)
            .color(ui::theme::TEXT_MUTED);
        container(column![title, subtitle].spacing(8))
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .padding(20)
            .style(|_theme: &Theme| container::Style {
                background: Some(ui::theme::BACKGROUND.into()),
                ..container::Style::default()
            })
            .into()
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
            size: iced::Size::new(380.0, 520.0),
            resizable: false,
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
            }
            let _ = self.tray.set_icon(Some(self.icons.quiet.clone()));
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
        let _ = self.config.save();
    }
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
