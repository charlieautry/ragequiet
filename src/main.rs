#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alert;
mod audio;
mod detector;
mod engine;

use cpal::traits::StreamTrait;
use detector::{Detector, GREEN, GREY};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

#[derive(Debug)]
enum AppEvent {
    Color([u8; 3]),
    Menu(MenuEvent),
}

/// 32x32 filled circle in the given color; generated in code, no asset files.
fn make_icon(rgb: [u8; 3]) -> Icon {
    const S: i32 = 32;
    let mut rgba = Vec::with_capacity((S * S * 4) as usize);
    for y in 0..S {
        for x in 0..S {
            let (dx, dy) = (x - S / 2, y - S / 2);
            let a = if dx * dx + dy * dy <= (S / 2 - 1).pow(2) {
                255
            } else {
                0
            };
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
    }
    Icon::from_rgba(rgba, S as u32, S as u32).expect("valid icon buffer")
}

fn main() -> anyhow::Result<()> {
    // tao 0.37's `EventLoopBuilder::build` takes `&mut self`, so the builder
    // needs its own mutable binding before we can call `.build()` on it.
    let mut event_loop_builder = EventLoopBuilder::<AppEvent>::with_user_event();
    let event_loop = event_loop_builder.build();

    let menu_proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(AppEvent::Menu(e));
    }));

    let enabled_item = CheckMenuItem::new("Enabled", true, true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append_items(&[&enabled_item, &PredefinedMenuItem::separator(), &quit_item])?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Ragequiet")
        .with_icon(make_icon(GREEN))
        .build()?;

    let mut detector = Detector::new();
    let start = std::time::Instant::now();
    let resume_flag = Arc::new(AtomicBool::new(false));
    let callback_resume_flag = Arc::clone(&resume_flag);
    let audio_proxy = event_loop.create_proxy();
    let stream = audio::start_input(move |frame| {
        if callback_resume_flag.swap(false, Ordering::Relaxed) {
            detector.resume();
        }
        let now_ms = start.elapsed().as_millis() as u64;
        let outcome = detector.on_frame(frame, now_ms);
        if outcome.beep {
            alert::play_beep();
        }
        if let Some(color) = outcome.color_change {
            let _ = audio_proxy.send_event(AppEvent::Color(color));
        }
    })?;
    stream.play()?;

    let mut enabled = true;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::UserEvent(app_event) = event {
            match app_event {
                AppEvent::Color(rgb) => {
                    if enabled {
                        let _ = tray.set_icon(Some(make_icon(rgb)));
                    }
                }
                AppEvent::Menu(e) => {
                    if e.id() == quit_item.id() {
                        *control_flow = ControlFlow::Exit;
                    } else if e.id() == enabled_item.id() {
                        enabled = enabled_item.is_checked();
                        if enabled {
                            resume_flag.store(true, Ordering::Relaxed);
                            let _ = stream.play();
                            let _ = tray.set_icon(Some(make_icon(GREEN)));
                        } else {
                            let _ = stream.pause();
                            let _ = tray.set_icon(Some(make_icon(GREY)));
                        }
                    }
                }
            }
        }
    });
}
