mod audio;
mod engine;

use cpal::traits::StreamTrait;
use engine::features;

fn main() -> anyhow::Result<()> {
    let stream = audio::start_input(|frame| {
        let db = features::db_from_rms(features::rms(frame));
        println!("{db:7.1} dB");
    })?;
    stream.play()?;
    println!("listening on default input — Ctrl+C to quit");
    std::thread::park();
    Ok(())
}
