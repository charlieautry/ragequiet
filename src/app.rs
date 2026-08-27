use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{HostTrait, StreamTrait};
use futures::SinkExt;
use iced::widget::column;
use iced::{window, Element, Subscription, Task, Theme};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::alert::AlertGate;
use crate::audio;
use crate::autostart;
use crate::bridge::{self, Command, CommandTx, MeasurementKind, SharedLevels};
use crate::config::{AlertSound, CalibrationState, Config, DeviceCalibration};
use crate::decode;
use crate::detector::{Detector, MeasurementUpdate, TrayState};
use crate::engine::Engine;
use crate::player::{PlayRequest, Player};
use crate::sounds::{self, BuiltinSound};
use crate::sysinfo;
use crate::ui;
use crate::ui::calibrate::{Wizard, WizardEvent, WizardResult, WizardStep};
use crate::ui::icons::{TrayIconSets, TrayIcons};

/// Rare events pushed from the audio callback to the UI.
enum AudioEvent {
    State(TrayState),
    Beeped,
    /// Progress/completion of a calibration measurement running inside the
    /// callback. Rare by construction: whole-percent changes only, and only
    /// while the wizard has one in flight.
    Measurement(MeasurementUpdate),
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
    recalibrate: MenuId,
    settings: MenuId,
    quit: MenuId,
}

/// All six built-in alert sounds, rendered once at boot so a play request is
/// just an `Arc` clone rather than re-synthesizing samples on every alert.
struct SoundCache {
    entries: [(BuiltinSound, Arc<Vec<f32>>); sounds::ALL.len()],
}

impl SoundCache {
    fn render_all() -> Self {
        Self {
            entries: sounds::ALL.map(|s| (s, Arc::new(sounds::render(s)))),
        }
    }

    fn get(&self, sound: BuiltinSound) -> Arc<Vec<f32>> {
        self.entries
            .iter()
            .find(|(s, _)| *s == sound)
            .map(|(_, samples)| Arc::clone(samples))
            .expect("SoundCache::render_all covers every BuiltinSound")
    }
}

/// One entry in the settings window's "Alert sound" `pick_list`: the six
/// built-ins plus a "Custom…" entry that (when picked) opens the file
/// dialog rather than selecting a sound directly. `Custom(Some(name))` is
/// never an option in the list itself — it only ever appears as the picker's
/// *current selection* once a custom file is active, so the closed control
/// shows the file's name instead of the literal "Custom…" placeholder text
/// (`pick_list` renders `selected.to_string()` independently of whether it
/// matches one of the listed options).
#[derive(Debug, Clone, PartialEq)]
pub enum SoundChoice {
    Builtin(BuiltinSound),
    Custom(Option<String>),
}

impl std::fmt::Display for SoundChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SoundChoice::Builtin(b) => write!(f, "{}", b.label()),
            SoundChoice::Custom(None) => write!(f, "Custom…"),
            SoundChoice::Custom(Some(name)) => write!(f, "{name}"),
        }
    }
}

/// The base filename of a custom sound's stored path, for display in the
/// picker's closed state (the full path is often too long for the control).
fn custom_sound_display_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .map(|f| f.to_string())
        .unwrap_or_else(|| path.to_string())
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
    icons: TrayIconSets,
    /// (level_db, threshold_db, peak_db, drift_db) sampled on Tick so `view`
    /// stays pure.
    pub(crate) latest: (f32, f32, f32, f32),
    /// Last tray state seen, for the settings window's status line.
    pub(crate) latest_tray_state: TrayState,
    /// Some while the calibration wizard owns the settings window's body.
    pub(crate) wizard: Option<Wizard>,
    /// True once a `*Changed` message has edited `config` since the last save
    /// (slider release, or a non-drag edit like Ctrl+scroll/arrow keys that
    /// never fires `on_release`); `commit_config` is the only place this is
    /// cleared.
    config_dirty: bool,
    /// True when the active device's calibration is missing, `BaselineOnly`,
    /// or degrades to uncalibrated `Tuning` — drives both the tray's dot icon
    /// and the settings banner. Recomputed at boot and after
    /// `apply_calibration`.
    pub(crate) calibration_incomplete: bool,
    /// Today's local date ("YYYY-MM-DD"), cached on `Tick`/`WindowOpened` so
    /// `view` never needs a syscall.
    pub(crate) today: String,
    /// Local wall-clock hour (0-23), cached the same way as `today`.
    pub(crate) hour: u32,
    /// Session-only dismissal of the drift-staleness nudge; never resets
    /// (unlike the once-per-day banner, which tracks a date).
    pub(crate) drift_nudge_dismissed: bool,
    /// All six built-in sounds, rendered once at boot.
    sounds: SoundCache,
    /// Owns the alert output stream's lifecycle on its own thread; `Beeped`
    /// hands it play requests and never blocks.
    player: Player,
    /// The decoded custom alert file (mono f32 at its native rate) when
    /// `config.alert_sound` is `Custom` and decoding succeeded; `None` means
    /// `current_sound` falls back to the soft beep (no custom sound
    /// configured, or the last decode attempt failed).
    custom_sound: Option<Arc<Vec<f32>>>,
    /// Native sample rate of `custom_sound`; meaningless while it's `None`.
    custom_sound_rate: u32,
    /// Inline settings-UI error from the most recent failed custom-sound
    /// decode (boot-time load, or a freshly picked file); cleared on the next
    /// successful decode.
    pub(crate) sound_error: Option<String>,
    /// Output device names, enumerated once each time the settings window
    /// opens (never per-frame/per-Tick). "System default" is prepended in
    /// the view, not stored here.
    pub(crate) output_devices: Vec<String>,
    /// Whether launch-at-login is currently on, per the registry (the
    /// runtime source of truth — see `crate::autostart`). Refreshed from
    /// `autostart::autostart_enabled()` at boot and on every `WindowOpened`,
    /// same pattern as `output_devices`.
    pub(crate) autostart: bool,
    /// Inline settings-UI error from the most recent failed `set_autostart`
    /// call; cleared on the next successful toggle. Mirrors `sound_error`.
    pub(crate) autostart_error: Option<String>,
    /// Inline settings-UI note shown when the tray "Recalibrate"/banner
    /// Calibrate buttons are pressed with no input device open. Cleared the
    /// next time a wizard is actually started.
    pub(crate) wizard_error: Option<String>,
    /// Set at `WizardStarted` when the default input device's name no longer
    /// matches `device_name` (the stream itself stays on the old device — see
    /// the wizard's Intro step, which renders the honest note when this is
    /// true).
    pub(crate) device_changed_note: bool,
    /// The `Measuring` step's progress value as of the last time it actually
    /// changed (either a fresh `Progress` event, or the last `Tick` that
    /// noticed a change) — compared against on every `Tick` to detect a
    /// stalled (voice-not-heard) measurement.
    measure_progress_seen: f32,
    /// Ticks (each ~80 ms) since `measure_progress_seen` last changed, while
    /// a voiced `Measuring` step is running; drives `stall_hint_visible`.
    pub(crate) measure_stalled_ticks: u32,
    /// This process's own CPU time samples for the settings window's CPU
    /// readout, updated each `Tick`; `None` until the first sample lands.
    cpu_sample: Option<(u64, u64)>,
    /// Fixed reference instant for converting `Instant::elapsed()` into the
    /// same 100-ns units `GetProcessTimes` reports in.
    cpu_epoch: std::time::Instant,
    /// Exponentially smoothed CPU percent (`smoothed = smoothed*0.7 + pct*0.3`)
    /// shown in the settings window; stays at 0.0 on non-Windows targets
    /// (`sysinfo::process_cpu_100ns` returns `None` there).
    pub(crate) cpu_percent_smoothed: f32,
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
    /// A calibration measurement update straight off the audio thread; routed
    /// into the wizard by `update` (and dropped when no wizard is running).
    Measurement(MeasurementUpdate),
    /// Enter the wizard (tray "Recalibrate"), opening the window if needed.
    WizardStarted,
    /// A wizard-state-machine transition: button presses (Begin/Continue/
    /// SkipCeiling) and audio-driven Progress/Complete alike.
    WizardEvent(WizardEvent),
    /// Leave the wizard, cancelling any in-flight measurement.
    WizardCancelled,
    /// Accept the finished result: persist it and apply it live.
    WizardFinished,
    /// Dismiss the once-per-day calibration-incomplete banner: records
    /// today's date so it stays hidden until tomorrow.
    BannerDismissed,
    /// Dismiss the drift-staleness nudge for this session only.
    DriftNudgeDismissed,
    /// The alert-sound `pick_list` changed: either a built-in was chosen
    /// directly, or the "Custom…" entry was picked (which itself triggers
    /// `PickCustomSound` rather than changing the sound right away).
    AlertSoundPicked(SoundChoice),
    /// Open the native file dialog for choosing a custom alert sound.
    PickCustomSound,
    /// The file dialog resolved; `None` means the user cancelled it.
    CustomSoundChosen(Option<PathBuf>),
    /// Alert volume slider drag; persisted on release like hold/cooldown.
    AlertVolumeChanged(f32),
    /// Output device `pick_list` changed; `None` selects the system default.
    OutputDevicePicked(Option<String>),
    /// The settings window's Test button: plays the configured sound at the
    /// configured volume/device, independent of the detector.
    TestSound,
    /// The "Start with Windows" toggler changed; calls `set_autostart`
    /// before touching any state (see the handler for the error path).
    AutostartToggled(bool),
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
        let (tuning_meta, calibration_incomplete) = device_meta(&config, &device_name);
        let tuning = crate::engine::Tuning {
            noise_floor_db: tuning_meta.noise_floor_db,
            quiet_db: tuning_meta.quiet_db,
            ceiling_db: tuning_meta.ceiling_db,
            sensitivity: tuning_meta.sensitivity,
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
            callback_shared.store(
                detector.last_level_db(),
                threshold_db,
                detector.peak_db(),
                detector.drift_db(),
            );
            if outcome.beep {
                let _ = events.send(AudioEvent::Beeped);
            }
            if let Some(state) = outcome.state_change {
                let _ = events.send(AudioEvent::State(state));
            }
            if let Some(update) = outcome.measurement {
                let _ = events.send(AudioEvent::Measurement(update));
            }
        })
        .ok();
        if let Some(stream) = &stream {
            let _ = stream.play();
        }

        let enabled_item = CheckMenuItem::new("Enabled", true, true, None);
        let recalibrate_item = MenuItem::new("Recalibrate", true, None);
        let settings_item = MenuItem::new("Settings", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        let menu_ids = MenuIds {
            enabled: enabled_item.id().clone(),
            recalibrate: recalibrate_item.id().clone(),
            settings: settings_item.id().clone(),
            quit: quit_item.id().clone(),
        };
        let menu = Menu::new();
        let _ = menu.append_items(&[
            &enabled_item,
            &recalibrate_item,
            &settings_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ]);

        let sounds = SoundCache::render_all();
        let player = Player::spawn();

        // A configured custom sound is decoded once, up front, so `Beeped`
        // never blocks on file IO: `current_sound` just clones the cached
        // Arc. A failed decode falls back to the soft beep and remembers an
        // inline error for the settings window rather than failing boot.
        let (custom_sound, custom_sound_rate, sound_error) = match &config.alert_sound {
            AlertSound::Custom { path } => match decode::decode_file(Path::new(path)) {
                Ok((samples, rate)) => (Some(Arc::new(samples)), rate, None),
                Err(e) => (None, sounds::SOUND_RATE, Some(format!("Could not load {path}: {e}"))),
            },
            AlertSound::Builtin(_) => (None, sounds::SOUND_RATE, None),
        };

        let icons = TrayIconSets::load();
        let initial_icon_set = if calibration_incomplete { &icons.dotted } else { &icons.plain };
        // No stream means nothing is being monitored: say so with the off icon.
        let initial_icon = if stream.is_some() {
            initial_icon_set.quiet.clone()
        } else {
            initial_icon_set.off.clone()
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
                latest: (-100.0, f32::NAN, -100.0, f32::NAN),
                latest_tray_state: TrayState::Quiet,
                wizard: None,
                config_dirty: false,
                calibration_incomplete,
                today: local_date(),
                hour: local_hour(),
                drift_nudge_dismissed: false,
                sounds,
                player,
                custom_sound,
                custom_sound_rate,
                sound_error,
                output_devices: Vec::new(),
                autostart: autostart::autostart_enabled(),
                autostart_error: None,
                wizard_error: None,
                device_changed_note: false,
                measure_progress_seen: 0.0,
                measure_stalled_ticks: 0,
                cpu_sample: None,
                cpu_epoch: std::time::Instant::now(),
                cpu_percent_smoothed: 0.0,
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
                } else if id == self.menu_ids.recalibrate {
                    return Task::done(Message::WizardStarted);
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
                    let _ = self.tray.set_icon(Some(self.active_icons().for_state(state)));
                }
                Task::none()
            }
            Message::Beeped => {
                self.alerts_this_session += 1;
                let (samples, source_rate) = self.current_sound();
                self.player.play(PlayRequest {
                    samples,
                    source_rate,
                    volume: self.config.effective_volume(),
                    device_name: self.config.output_device.clone(),
                });
                Task::none()
            }
            Message::WindowOpened(id) => {
                self.settings_window = Some(id);
                // First paint uses whatever the audio thread has actually
                // published, instead of the boot-time silence sentinel —
                // avoids a stale reading from before the window existed.
                self.latest = self.shared.load();
                self.today = local_date();
                self.hour = local_hour();
                self.output_devices = enumerate_output_devices();
                // The registry is the runtime source of truth: re-read it
                // every time the window opens so an out-of-band change
                // (e.g. hand-edited via regedit) is reflected rather than
                // trusting a possibly-stale cached value.
                self.autostart = autostart::autostart_enabled();
                round_corners(id)
            }
            Message::WindowClosed(id) => {
                if self.settings_window == Some(id) {
                    self.settings_window = None;
                    // The wizard lives in this window: closing it must not
                    // silently discard a finished calibration (a completed
                    // wizard is applied), and must not leave a measurement
                    // running headless (any in-flight one is cancelled).
                    self.teardown_wizard(true);
                }
                self.commit_config();
                Task::none()
            }
            Message::Tick => {
                self.latest = self.shared.load();
                self.today = local_date();
                self.hour = local_hour();

                // Voiced-measurement stall detection: a `Progress` event
                // resets `measure_progress_seen`/`measure_stalled_ticks`
                // (see `Message::Measurement`), so seeing the same progress
                // again here means no new progress arrived since — the user
                // isn't being heard.
                if let Some(wizard) = &self.wizard
                    && let WizardStep::Measuring { kind, progress } = wizard.step
                {
                    if kind == MeasurementKind::NoiseFloor {
                        self.measure_stalled_ticks = 0;
                    } else if progress == self.measure_progress_seen {
                        self.measure_stalled_ticks = self.measure_stalled_ticks.saturating_add(1);
                    } else {
                        self.measure_progress_seen = progress;
                        self.measure_stalled_ticks = 0;
                    }
                }

                // CPU readout: only sampled while a window exists (Tick's
                // own subscription is gated the same way), so this never
                // costs anything with the app parked in the tray.
                if let Some(cpu_100ns) = sysinfo::process_cpu_100ns() {
                    let wall_100ns = (self.cpu_epoch.elapsed().as_nanos() / 100) as u64;
                    let cur = (cpu_100ns, wall_100ns);
                    if let Some(prev) = self.cpu_sample {
                        let pct = sysinfo::cpu_percent(prev, cur);
                        self.cpu_percent_smoothed = self.cpu_percent_smoothed * 0.7 + pct * 0.3;
                    }
                    self.cpu_sample = Some(cur);
                }

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
            Message::Measurement(update) => {
                // No wizard means this is a straggler from a cancelled run:
                // drop it rather than let it reach the state machine.
                if self.wizard.is_none() {
                    return Task::none();
                }
                let event = match update {
                    MeasurementUpdate::Progress(p) => {
                        // Real progress just arrived: the stall clock resets.
                        self.measure_progress_seen = p;
                        self.measure_stalled_ticks = 0;
                        WizardEvent::Progress(p)
                    }
                    MeasurementUpdate::Complete(kind, db) => WizardEvent::Complete(kind, db),
                };
                Task::done(Message::WizardEvent(event))
            }
            Message::WizardStarted => {
                // No input device means there is nothing to calibrate
                // against: show the settings window (so the message is
                // visible) but never actually enter the wizard, which would
                // just sit measuring a silence that's really "no stream".
                if !self.has_stream() {
                    self.wizard_error =
                        Some("No microphone available — connect one and try again.".into());
                    return self.open_settings();
                }
                self.wizard_error = None;

                // Restarting mid-run must not orphan a measurement in the
                // audio callback, and must not silently apply a stale
                // finished result either — restarting is the user choosing
                // to redo it, not to keep what they had.
                self.teardown_wizard(false);

                // The default input device can change between launch and
                // now (a headset plugged in, a USB mic unplugged): re-key
                // the meter's markers off whatever's current rather than
                // silently mis-keying against the launch-time device. The
                // audio stream itself stays on the original device (a full
                // rebuild is out of scope for v1.0) — the Intro step's note
                // says so honestly rather than pretending it followed along.
                let current = audio::default_input_name().unwrap_or_else(|| "unknown input".into());
                self.device_changed_note = current != self.device_name;
                if self.device_changed_note {
                    self.device_name = current;
                    self.refresh_device_meta();
                }

                self.measure_progress_seen = 0.0;
                self.measure_stalled_ticks = 0;

                let _ = self.commands.send(Command::SetWizardActive(true));
                self.wizard = Some(Wizard::new(local_hour()));
                // Started from the tray with no window: the wizard has to be
                // visible to be usable, so open one (no-op if already open).
                self.open_settings()
            }
            Message::WizardEvent(event) => {
                if let Some(wizard) = self.wizard.as_mut()
                    && let Some(kind) = wizard.on_event(event)
                {
                    // The `Measurement` is allocated here, on the UI thread —
                    // the audio callback only installs the finished value.
                    let measurement = crate::detector::measurement_for(kind);
                    let _ = self
                        .commands
                        .send(Command::StartMeasurement(kind, Box::new(measurement)));
                }
                Task::none()
            }
            Message::WizardCancelled => {
                // The user chose to discard (Cancel row, or the Done step's
                // explicit Discard button): a finished result is not applied.
                self.teardown_wizard(false);
                Task::none()
            }
            Message::WizardFinished => {
                let result = match self.wizard.as_ref().map(|w| &w.step) {
                    Some(WizardStep::Done { result }) => Some(*result),
                    _ => None, // only a finished wizard has anything to apply
                };
                if let Some(result) = result {
                    self.apply_calibration(result);
                    self.wizard = None;
                    // Finish is an exit from the wizard just like the other
                    // paths: the alert suppression it started on `WizardStarted`
                    // must not outlive it.
                    let _ = self.commands.send(Command::SetWizardActive(false));
                }
                Task::none()
            }
            Message::BannerDismissed => {
                self.config.banner_dismissed_on = Some(local_date());
                // Direct save, like `apply_calibration`: a dismissal must
                // survive even if the app never reaches a commit point.
                let _ = self.config.save();
                Task::none()
            }
            Message::DriftNudgeDismissed => {
                self.drift_nudge_dismissed = true;
                Task::none()
            }
            Message::AlertSoundPicked(choice) => {
                match choice {
                    SoundChoice::Builtin(b) => {
                        self.config.alert_sound = AlertSound::Builtin(b);
                        self.config_dirty = true;
                        self.commit_config();
                        Task::none()
                    }
                    // Picking "Custom…" itself doesn't select a sound: it
                    // opens the file dialog, which only commits a change on
                    // a successful pick + decode (`CustomSoundChosen`).
                    SoundChoice::Custom(_) => Task::done(Message::PickCustomSound),
                }
            }
            Message::PickCustomSound => {
                // `pick_file()` must be called here, synchronously on the UI
                // thread, to spawn the native dialog; the returned future
                // (awaited by `Task::perform` on iced's executor) is what
                // resolves once the user closes it.
                let dialog = rfd::AsyncFileDialog::new().add_filter("Audio", &["wav", "mp3"]).pick_file();
                Task::perform(dialog, |handle| {
                    Message::CustomSoundChosen(handle.map(|h| h.path().to_path_buf()))
                })
            }
            Message::CustomSoundChosen(Some(path)) => {
                match decode::decode_file(&path) {
                    Ok((samples, rate)) => {
                        self.custom_sound = Some(Arc::new(samples));
                        self.custom_sound_rate = rate;
                        self.sound_error = None;
                        self.config.alert_sound = AlertSound::Custom { path: path.to_string_lossy().into_owned() };
                        self.config_dirty = true;
                        self.commit_config();
                    }
                    Err(e) => {
                        // Keep whatever sound was already configured/playing;
                        // only the inline error line changes.
                        self.sound_error = Some(format!("Could not load {}: {e}", path.display()));
                    }
                }
                Task::none()
            }
            // The dialog was cancelled: nothing changes.
            Message::CustomSoundChosen(None) => Task::none(),
            Message::AlertVolumeChanged(value) => {
                self.config.alert_volume = value;
                self.config_dirty = true;
                Task::none()
            }
            Message::OutputDevicePicked(device) => {
                self.config.output_device = device;
                self.config_dirty = true;
                self.commit_config();
                Task::none()
            }
            Message::TestSound => {
                let (samples, source_rate) = self.current_sound();
                self.player.play(PlayRequest {
                    samples,
                    source_rate,
                    volume: self.config.effective_volume(),
                    device_name: self.config.output_device.clone(),
                });
                Task::none()
            }
            Message::AutostartToggled(enabled) => {
                // Registry write first: only reflect the new state (cache +
                // config + commit) on success, and never flip the checkbox
                // on failure — the UI must stay honest about what's
                // actually in the registry.
                match autostart::set_autostart(enabled) {
                    Ok(()) => {
                        self.autostart = enabled;
                        self.autostart_error = None;
                        self.config.start_with_windows = enabled;
                        self.config_dirty = true;
                        self.commit_config();
                    }
                    Err(e) => {
                        self.autostart_error = Some(format!("Could not update Windows startup: {e}"));
                    }
                }
                Task::none()
            }
            Message::Quit => {
                // Quit is an exit path like any other: it must not silently
                // discard a finished calibration sitting on the Done screen.
                self.teardown_wizard(true);
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
            size: iced::Size::new(400.0, 600.0),
            resizable: false,
            decorations: false,
            icon: Some(ui::icons::window_icon()),
            ..window::Settings::default()
        });
        open.map(Message::WindowOpened)
    }

    /// The tray icon set to draw from: the dotted variant whenever the active
    /// device's calibration is incomplete, otherwise the plain one.
    fn active_icons(&self) -> &TrayIcons {
        if self.calibration_incomplete {
            &self.icons.dotted
        } else {
            &self.icons.plain
        }
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
                let _ = self.tray.set_icon(Some(self.active_icons().quiet.clone()));
            } else {
                let _ = self.tray.set_icon(Some(self.active_icons().off.clone()));
            }
            // Drop any stale state from before the toggle (e.g. Loud lingering
            // across a disable/enable) so the status line doesn't flash a
            // leftover reading it never re-earned.
            self.latest_tray_state = TrayState::Quiet;
        } else {
            if let Some(stream) = &self.stream {
                let _ = stream.pause();
            }
            let _ = self.tray.set_icon(Some(self.active_icons().off.clone()));
        }
    }

    /// Drop the wizard, telling the audio thread to abandon any measurement it
    /// is still accumulating. Safe to call with no wizard running.
    fn cancel_wizard(&mut self) {
        if let Some(wizard) = self.wizard.take()
            && wizard.cancelled_measurement().is_some()
        {
            let _ = self.commands.send(Command::CancelMeasurement);
        }
    }

    /// Leave the wizard on any exit path (window close, quit, an explicit
    /// cancel/discard, or a restart). When `apply_if_done` is true and the
    /// wizard had reached `Done`, the finished calibration is persisted
    /// rather than thrown away — closing the window or quitting must never
    /// silently discard a completed calibration. Explicit discard (the Done
    /// screen's Discard button, the ordinary Cancel row, or a restart) always
    /// passes false: the user chose to abandon it.
    fn teardown_wizard(&mut self, apply_if_done: bool) {
        if apply_if_done
            && let Some(WizardStep::Done { result }) = self.wizard.as_ref().map(|w| &w.step)
        {
            let result = *result;
            self.apply_calibration(result);
        }
        self.cancel_wizard();
        let _ = self.commands.send(Command::SetWizardActive(false));
    }

    /// Persist a finished calibration and make it live: config write, engine
    /// retune, and a `tuning_meta` refresh so the meter's markers and the
    /// sensitivity slider's gating are correct on the very next paint — no
    /// restart.
    fn apply_calibration(&mut self, result: WizardResult) {
        let prior_sensitivity = self
            .config
            .calibration
            .get(&self.device_name)
            .map(|c| c.sensitivity);
        let entry = calibration_from_result(&result, prior_sensitivity);

        // Round-trip through `tuning()` exactly as boot does, so a nonsense
        // measurement degrades the same way a hand-edited config would.
        let tuning = entry.tuning();
        self.tuning_meta = TuningMeta {
            noise_floor_db: tuning.noise_floor_db,
            quiet_db: tuning.quiet_db,
            ceiling_db: tuning.ceiling_db,
            ceiling_confirmed: entry.state != CalibrationState::BaselineOnly,
            sensitivity: tuning.sensitivity,
        };
        let _ = self.commands.send(Command::SetTuning(tuning));

        self.config.calibration.insert(self.device_name.clone(), entry);
        // Written straight through rather than via the dirty/commit flow: a
        // finished calibration must survive even if the app never reaches a
        // commit point.
        let _ = self.config.save();

        // Recompute the dot/banner flag and repaint the tray immediately —
        // a calibration finishing must not wait for the next state change to
        // stop showing the dot.
        self.calibration_incomplete = calibration_incomplete_for(&self.config, &self.device_name);
        let icon = if self.enabled && self.stream.is_some() {
            self.active_icons().for_state(self.latest_tray_state)
        } else {
            self.active_icons().off.clone()
        };
        let _ = self.tray.set_icon(Some(icon));
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

    /// Re-derives `tuning_meta`/`calibration_incomplete` for `device_name`
    /// after `Message::WizardStarted` notices it changed underneath a
    /// running stream — mirrors `boot`'s computation via the same
    /// `device_meta` helper so the two never drift apart.
    fn refresh_device_meta(&mut self) {
        let (meta, incomplete) = device_meta(&self.config, &self.device_name);
        self.tuning_meta = meta;
        self.calibration_incomplete = incomplete;
    }

    /// Whether the wizard's `Measuring` step should show the "waiting to
    /// hear your voice" hint under its progress bar — `false` outside a
    /// voiced `Measuring` step (see `stall_hint_visible`).
    pub(crate) fn wizard_stall_hint(&self) -> bool {
        match self.wizard.as_ref().map(|w| &w.step) {
            Some(WizardStep::Measuring { kind, .. }) => {
                stall_hint_visible(*kind != MeasurementKind::NoiseFloor, self.measure_stalled_ticks)
            }
            _ => false,
        }
    }

    /// Resolves the currently configured alert sound to samples + rate for a
    /// `PlayRequest`: the built-in cache for `AlertSound::Builtin`, or the
    /// decoded custom buffer for `AlertSound::Custom` — falling back to the
    /// soft beep whenever a custom sound is configured but hasn't (or
    /// couldn't) decode (`custom_sound` is `None`), so a beep never goes
    /// silent just because a file went missing.
    pub(crate) fn current_sound(&self) -> (Arc<Vec<f32>>, u32) {
        match &self.config.alert_sound {
            AlertSound::Builtin(b) => (self.sounds.get(*b), sounds::SOUND_RATE),
            AlertSound::Custom { .. } => match &self.custom_sound {
                Some(samples) => (Arc::clone(samples), self.custom_sound_rate),
                None => (self.sounds.get(BuiltinSound::SoftBeep), sounds::SOUND_RATE),
            },
        }
    }

    /// The `SoundChoice` the alert-sound `pick_list` should show as selected:
    /// the matching builtin, or (for a custom sound) its file's display name
    /// — never `Custom(None)`, which only appears as the "Custom…" option
    /// itself.
    pub(crate) fn sound_choice(&self) -> SoundChoice {
        match &self.config.alert_sound {
            AlertSound::Builtin(b) => SoundChoice::Builtin(*b),
            AlertSound::Custom { path } => SoundChoice::Custom(Some(custom_sound_display_name(path))),
        }
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

/// Pure core of [`App::apply_calibration`]: the wizard's three numbers plus
/// whatever sensitivity the device already had become a persistable entry.
/// A measured ceiling is what separates `CeilingSet` from `BaselineOnly`
/// (the engine estimates a margin for the latter).
fn calibration_from_result(result: &WizardResult, prior_sensitivity: Option<f32>) -> DeviceCalibration {
    DeviceCalibration {
        state: if result.ceiling_db.is_some() {
            CalibrationState::CeilingSet
        } else {
            CalibrationState::BaselineOnly
        },
        quiet_db: result.quiet_db,
        ceiling_db: result.ceiling_db,
        noise_floor_db: result.noise_floor_db,
        // Recalibrating re-measures the anchors, not the user's taste: an
        // existing sensitivity preference survives.
        sensitivity: prior_sensitivity.unwrap_or(0.5),
    }
}

/// Computes the meter's markers (`TuningMeta`) and the calibration-incomplete
/// flag for a device from its calibration entry (or lack of one). Shared by
/// `boot` and `App::refresh_device_meta` (the wizard's device re-read) so a
/// device swap mid-run can't silently mis-key either one against the wrong
/// entry.
fn device_meta(config: &Config, device_name: &str) -> (TuningMeta, bool) {
    let calibration = config.calibration.get(device_name);
    let tuning = calibration.map(|c| c.tuning()).unwrap_or_default();
    let incomplete = calibration_incomplete_for(config, device_name);
    let meta = TuningMeta {
        noise_floor_db: tuning.noise_floor_db,
        quiet_db: tuning.quiet_db,
        ceiling_db: tuning.ceiling_db,
        ceiling_confirmed: matches!(
            calibration.map(|c| c.state),
            Some(CalibrationState::CeilingSet) | Some(CalibrationState::CeilingLearned)
        ),
        sensitivity: tuning.sensitivity,
    };
    (meta, incomplete)
}

/// True when the active device's calibration is missing, `BaselineOnly`, or
/// degrades to an uncalibrated `Tuning` (e.g. a hand-edited or half-parsed
/// config entry) — the single source of truth for both the tray's dot icon
/// and the settings banner.
fn calibration_incomplete_for(config: &Config, device_name: &str) -> bool {
    match config.calibration.get(device_name) {
        None => true,
        Some(entry) => {
            entry.state == CalibrationState::BaselineOnly || entry.tuning().quiet_db.is_none()
        }
    }
}

/// Output device names, for the settings window's "Output device" picker.
/// Enumerated fresh each time the window opens (see `Message::WindowOpened`)
/// rather than cached for the app's lifetime, so a device plugged in after
/// launch still shows up without a restart; never called per-frame.
fn enumerate_output_devices() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

/// Whether the once-per-day calibration-incomplete banner should be visible.
/// Pure and parameterized so the date/hour syscalls stay out of `view`.
pub fn banner_visible(incomplete: bool, dismissed_on: Option<&str>, today: &str) -> bool {
    incomplete && dismissed_on != Some(today)
}

/// Whether the "mic level shifted" staleness nudge should be visible. Only
/// meaningful once calibration is complete (an incomplete calibration shows
/// its own banner instead, which always wins — see `ui::settings::view`).
/// 6.0 dB is well above the engine's +3 dB threshold clamp, so ordinary calm
/// speech (whose baseline commonly settles a few dB above the calibrated
/// quiet point, since the feed cutoff sits a few dB higher still) never
/// reaches it; only a genuine level shift — a gain change or a moved mic —
/// does.
pub fn drift_nudge_visible(incomplete: bool, drift_db: f32, dismissed: bool) -> bool {
    !incomplete && !dismissed && drift_db.is_finite() && drift_db >= 6.0
}

/// Whether a voiced (`QuietPoint`/`Ceiling`) measurement's "waiting to hear
/// your voice" hint should show under the wizard's progress bar: true once
/// progress has sat still for more than the stall threshold (60 Ticks x
/// 80 ms ~= 5 s). `NoiseFloor` never shows it — that step wants silence, not
/// speech, so a stalled 0% there is expected, not a problem.
pub fn stall_hint_visible(kind_is_voiced: bool, stalled_ticks: u32) -> bool {
    kind_is_voiced && stalled_ticks > 60
}

/// Local wall-clock hour (0-23). Only the wizard's late-night default reads
/// it, and it takes the hour as a parameter, so this stays a leaf function
/// with no pure logic inside it.
#[cfg(windows)]
fn local_hour() -> u32 {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    let mut now = SYSTEMTIME::default();
    // SAFETY: `GetLocalTime` only writes a full `SYSTEMTIME` through the
    // pointer we own here; it cannot fail.
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&raw mut now) };
    u32::from(now.wHour)
}

/// Midday: on non-Windows targets there is no tray app anyway, and midday is
/// the hour that disables the late-night skip default.
#[cfg(not(windows))]
fn local_hour() -> u32 {
    12
}

/// Today's local date as an ISO string ("2026-08-27"), for the once-per-day
/// banner-dismissal comparison. Only the leaf reads the clock; `banner_visible`
/// takes the result as a plain `&str` parameter.
#[cfg(windows)]
fn local_date() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    let mut now = SYSTEMTIME::default();
    // SAFETY: `GetLocalTime` only writes a full `SYSTEMTIME` through the
    // pointer we own here; it cannot fail.
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&raw mut now) };
    format!("{:04}-{:02}-{:02}", now.wYear, now.wMonth, now.wDay)
}

#[cfg(not(windows))]
fn local_date() -> String {
    "1970-01-01".to_string()
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
                                AudioEvent::Measurement(update) => Message::Measurement(update),
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

    fn result(ceiling_db: Option<f32>) -> WizardResult {
        WizardResult { noise_floor_db: -58.0, quiet_db: -34.0, ceiling_db }
    }

    #[test]
    fn measured_ceiling_yields_ceiling_set() {
        let cal = calibration_from_result(&result(Some(-18.0)), None);
        assert_eq!(cal.state, CalibrationState::CeilingSet);
        assert_eq!(cal.quiet_db, -34.0);
        assert_eq!(cal.ceiling_db, Some(-18.0));
        assert_eq!(cal.noise_floor_db, -58.0);
    }

    #[test]
    fn skipped_ceiling_yields_baseline_only() {
        let cal = calibration_from_result(&result(None), None);
        assert_eq!(cal.state, CalibrationState::BaselineOnly);
        assert_eq!(cal.ceiling_db, None);
        assert_eq!(cal.quiet_db, -34.0);
    }

    #[test]
    fn existing_sensitivity_survives_recalibration() {
        let cal = calibration_from_result(&result(Some(-18.0)), Some(0.8));
        assert_eq!(cal.sensitivity, 0.8);
    }

    #[test]
    fn first_calibration_defaults_sensitivity_to_half() {
        let cal = calibration_from_result(&result(Some(-18.0)), None);
        assert_eq!(cal.sensitivity, 0.5);
    }

    #[test]
    fn banner_visible_when_incomplete_and_never_dismissed() {
        assert!(banner_visible(true, None, "2026-08-27"));
    }

    #[test]
    fn banner_hidden_same_day_after_dismissal() {
        assert!(!banner_visible(true, Some("2026-08-27"), "2026-08-27"));
    }

    #[test]
    fn banner_visible_again_next_day() {
        assert!(banner_visible(true, Some("2026-08-27"), "2026-08-28"));
    }

    #[test]
    fn banner_hidden_when_calibration_complete() {
        assert!(!banner_visible(false, None, "2026-08-27"));
        assert!(!banner_visible(false, Some("2026-08-26"), "2026-08-27"));
    }

    #[test]
    fn drift_nudge_hidden_while_calibration_incomplete() {
        assert!(!drift_nudge_visible(true, 5.0, false));
    }

    #[test]
    fn drift_nudge_hidden_below_the_threshold() {
        assert!(!drift_nudge_visible(false, 2.5, false));
        // At (and just past) the old threshold, and even at the engine's
        // own +3 dB clamp — ordinary speech reaches this, so it must stay
        // quiet.
        assert!(!drift_nudge_visible(false, 2.9, false));
        assert!(!drift_nudge_visible(false, 3.0, false));
        assert!(!drift_nudge_visible(false, 5.9, false));
    }

    #[test]
    fn drift_nudge_visible_at_or_above_threshold() {
        assert!(drift_nudge_visible(false, 6.0, false));
        assert!(drift_nudge_visible(false, 12.0, false));
    }

    #[test]
    fn drift_nudge_hidden_when_dismissed_this_session() {
        assert!(!drift_nudge_visible(false, 3.0, true));
    }

    #[test]
    fn drift_nudge_hidden_when_drift_is_nan() {
        assert!(!drift_nudge_visible(false, f32::NAN, false));
    }

    #[test]
    fn calibration_incomplete_when_no_entry_for_device() {
        let config = Config::default();
        assert!(calibration_incomplete_for(&config, "Mic"));
    }

    #[test]
    fn calibration_incomplete_when_baseline_only() {
        let mut config = Config::default();
        config.calibration.insert(
            "Mic".into(),
            DeviceCalibration {
                state: CalibrationState::BaselineOnly,
                quiet_db: -34.0,
                ceiling_db: None,
                noise_floor_db: -58.0,
                sensitivity: 0.5,
            },
        );
        assert!(calibration_incomplete_for(&config, "Mic"));
    }

    #[test]
    fn calibration_incomplete_when_tuning_degrades_to_uncalibrated() {
        let mut config = Config::default();
        config.calibration.insert(
            "Mic".into(),
            DeviceCalibration {
                state: CalibrationState::CeilingSet,
                quiet_db: f32::NAN, // half-parsed entry: degrades to uncalibrated
                ceiling_db: Some(-18.0),
                noise_floor_db: -58.0,
                sensitivity: 0.5,
            },
        );
        assert!(calibration_incomplete_for(&config, "Mic"));
    }

    #[test]
    fn custom_sound_display_name_strips_the_directory() {
        assert_eq!(custom_sound_display_name("C:/Users/me/Sounds/ding.wav"), "ding.wav");
        assert_eq!(custom_sound_display_name(r"C:\Users\me\Sounds\ding.wav"), "ding.wav");
    }

    #[test]
    fn custom_sound_display_name_falls_back_to_the_whole_path_when_unparseable() {
        // A bare trailing separator has no file_name component; the whole
        // string is a safer fallback than an empty label.
        assert_eq!(custom_sound_display_name("C:/"), "C:/");
    }

    #[test]
    fn sound_choice_display_matches_label_or_custom_text() {
        assert_eq!(SoundChoice::Builtin(BuiltinSound::Chime).to_string(), "Chime");
        assert_eq!(SoundChoice::Custom(None).to_string(), "Custom…");
        assert_eq!(SoundChoice::Custom(Some("ding.wav".to_string())).to_string(), "ding.wav");
    }

    #[test]
    fn stall_hint_hidden_for_noise_floor_regardless_of_ticks() {
        assert!(!stall_hint_visible(false, 0));
        assert!(!stall_hint_visible(false, 1000));
    }

    #[test]
    fn stall_hint_hidden_at_and_below_the_threshold() {
        assert!(!stall_hint_visible(true, 0));
        assert!(!stall_hint_visible(true, 60));
    }

    #[test]
    fn stall_hint_visible_just_past_the_threshold() {
        assert!(stall_hint_visible(true, 61));
        assert!(stall_hint_visible(true, 1000));
    }

    #[test]
    fn device_meta_reports_incomplete_with_no_calibration_entry() {
        let config = Config::default();
        let (meta, incomplete) = device_meta(&config, "Mic");
        assert!(incomplete);
        assert_eq!(meta.quiet_db, None);
    }

    #[test]
    fn device_meta_reflects_a_confirmed_ceiling() {
        let mut config = Config::default();
        config.calibration.insert(
            "Mic".into(),
            DeviceCalibration {
                state: CalibrationState::CeilingSet,
                quiet_db: -34.0,
                ceiling_db: Some(-18.0),
                noise_floor_db: -58.0,
                sensitivity: 0.7,
            },
        );
        let (meta, incomplete) = device_meta(&config, "Mic");
        assert!(!incomplete);
        assert_eq!(meta.quiet_db, Some(-34.0));
        assert_eq!(meta.ceiling_db, Some(-18.0));
        assert!(meta.ceiling_confirmed);
        assert_eq!(meta.sensitivity, 0.7);
    }

    #[test]
    fn calibration_complete_when_ceiling_set_with_valid_tuning() {
        let mut config = Config::default();
        config.calibration.insert(
            "Mic".into(),
            DeviceCalibration {
                state: CalibrationState::CeilingSet,
                quiet_db: -34.0,
                ceiling_db: Some(-18.0),
                noise_floor_db: -58.0,
                sensitivity: 0.5,
            },
        );
        assert!(!calibration_incomplete_for(&config, "Mic"));
    }
}
