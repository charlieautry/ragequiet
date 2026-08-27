#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alert;
mod app;
mod audio;
mod bridge;
mod config;
mod detector;
mod engine;
mod ui;

fn main() -> iced::Result {
    iced::daemon(app::App::boot, app::App::update, app::App::view)
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .title(app::App::title)
        .font(ui::theme::FONT_REGULAR_BYTES)
        .font(ui::theme::FONT_SEMIBOLD_BYTES)
        .default_font(ui::theme::FONT_REGULAR)
        .run()
}
