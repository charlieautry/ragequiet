mod alert;
mod audio;
mod engine;

use alert::AlertGate;
use cpal::traits::StreamTrait;
use engine::{Engine, State};

fn main() -> anyhow::Result<()> {
    let mut eng = Engine::new();
    let mut gate = AlertGate::new(300, 3000);
    let start = std::time::Instant::now();
    let stream = audio::start_input(move |frame| {
        let state = eng.process(frame);
        let over = matches!(state, State::TooLoud { .. });
        if gate.update(over, start.elapsed().as_millis() as u64) {
            alert::play_beep();
            println!("ALERT");
        }
    })?;
    stream.play()?;
    println!("listening — sustained loud voice beeps after 300 ms, 3 s cooldown");
    std::thread::park();
    Ok(())
}
