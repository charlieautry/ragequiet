#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alert;
mod app;
mod audio;
mod bridge;
mod config;
mod detector;
mod engine;

fn main() -> iced::Result {
    iced::daemon(app::App::boot, app::App::update, app::App::view)
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .title(app::App::title)
        .run()
}
