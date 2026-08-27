#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alert;
mod audio;
mod engine;

use alert::AlertGate;
use cpal::traits::StreamTrait;
use engine::{Engine, State};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

const GREEN: [u8; 3] = [46, 204, 113];
const YELLOW: [u8; 3] = [241, 196, 15];
const RED: [u8; 3] = [231, 76, 60];
const GREY: [u8; 3] = [127, 140, 141];

#[derive(Debug)]
enum AppEvent {
    Color([u8; 3]),
    Menu(MenuEvent),
}

fn color_for(state: State) -> [u8; 3] {
    match state {
        State::Quiet | State::Calm { .. } => GREEN,
        State::GettingLoud { .. } => YELLOW,
        State::TooLoud { .. } => RED,
    }
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

    let mut eng = Engine::new();
    let mut gate = AlertGate::new(300, 3000);
    let start = std::time::Instant::now();
    let mut last_color = GREEN;
    let audio_proxy = event_loop.create_proxy();
    let stream = audio::start_input(move |frame| {
        let state = eng.process(frame);
        if gate.update(
            matches!(state, State::TooLoud { .. }),
            start.elapsed().as_millis() as u64,
        ) {
            alert::play_beep();
        }
        // notify the UI thread only on change, so idle costs nothing
        let color = color_for(state);
        if color != last_color {
            last_color = color;
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
