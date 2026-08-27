mod audio;
mod engine;

use cpal::traits::StreamTrait;
use engine::{Engine, State};

fn main() -> anyhow::Result<()> {
    let mut eng = Engine::new();
    let stream = audio::start_input(move |frame| {
        match eng.process(frame) {
            State::Quiet => {}
            State::Calm { db } => println!("  calm {db:6.1} dB"),
            State::GettingLoud { db } => println!("  getting loud {db:6.1} dB"),
            State::TooLoud { db } => println!("LOUD {db:6.1} dB"),
        }
    })?;
    stream.play()?;
    println!("listening — talk normally for ~30 s to build a baseline, then raise your voice");
    std::thread::park();
    Ok(())
}
