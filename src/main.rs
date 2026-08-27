#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alert;
mod app;
mod audio;
mod autostart;
mod bridge;
mod config;
mod decode;
mod detector;
mod engine;
mod player;
mod sounds;
mod sysinfo;
mod ui;

fn main() -> iced::Result {
    // Single-instance guard: a second launch (e.g. from autostart racing a
    // manual start, or a user double-clicking the exe again) must exit
    // immediately rather than opening a second tray icon/audio stream.
    if !autostart::acquire_single_instance() {
        return Ok(());
    }

    iced::daemon(app::App::boot, app::App::update, app::App::view)
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .title(app::App::title)
        .font(ui::theme::FONT_REGULAR_BYTES)
        .font(ui::theme::FONT_SEMIBOLD_BYTES)
        .default_font(ui::theme::FONT_REGULAR)
        .run()
}
