//! Alert playback: a dedicated thread that owns a lazily-opened cpal output
//! stream. The audio (mic) callback never touches this — it only sends a
//! `PlayRequest` through an mpsc channel from the UI thread.
//!
//! Lifecycle: the worker blocks on `recv_timeout(10s)`. The first request
//! opens the output stream; subsequent requests reuse it (restarting
//! playback from the top — right for alerts, which never need to queue).
//! After 10s with nothing playing, the stream is dropped (closes the
//! device) so a quiet session costs nothing. Device/build failures are
//! logged once (deduped) and never kill the worker loop.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// A request to play one alert sound. Sent from the UI thread; never
/// blocks the caller.
pub struct PlayRequest {
    /// Mono samples at `source_rate`.
    pub samples: Arc<Vec<f32>>,
    pub source_rate: u32,
    /// 0..=1; clamped at write time regardless of what's passed in.
    pub volume: f32,
    /// `None` = system default output device.
    pub device_name: Option<String>,
}

/// Handle to the player's worker thread. Cheap to clone-by-reference (the
/// sender and the shared error slot are the only fields); `App` owns one.
pub struct Player {
    tx: mpsc::Sender<PlayRequest>,
    /// The worker's last open/build failure, if the most recent attempt
    /// failed; `None` once a subsequent open succeeds. This is the
    /// user-facing channel for device errors — `windows_subsystem` builds
    /// have no console for the worker's `eprintln!` to reach, so the
    /// settings window is the only place a Test-button failure (or a boot-
    /// time device loss) is ever visible.
    error: Arc<Mutex<Option<String>>>,
}

impl Player {
    /// Starts the worker thread and returns a handle to it. The thread
    /// parks in `recv_timeout` when idle and opens no device until the
    /// first `play()` call.
    pub fn spawn() -> Player {
        let (tx, rx) = mpsc::channel::<PlayRequest>();
        let error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&error);
        std::thread::spawn(move || worker_loop(rx, worker_error));
        Player { tx, error }
    }

    /// Non-blocking: hands the request to the worker thread. Safe to call
    /// from the UI thread. Silently dropped if the worker thread has
    /// somehow died (it shouldn't).
    pub fn play(&self, req: PlayRequest) {
        let _ = self.tx.send(req);
    }

    /// The worker's current error, if any (lock, clone, return). Polled by
    /// the UI on `Tick`/`WindowOpened` rather than pushed, since the worker
    /// thread has no direct line back into the iced update loop.
    pub fn last_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|guard| guard.clone())
    }
}

/// Shared playback cursor, read by the output stream's realtime callback
/// and written by the worker on each new request. Kept small; both sides
/// hold the lock only briefly (the worker just resets a handful of fields,
/// the callback does a bounded loop over one buffer).
struct Playback {
    samples: Arc<Vec<f32>>,
    volume: f32,
    step: f32,
    acc: f32,
    pos: usize,
    active: bool,
}

impl Playback {
    fn silent() -> Self {
        Self {
            samples: Arc::new(Vec::new()),
            volume: 0.0,
            step: 1.0,
            acc: 0.0,
            pos: 0,
            active: false,
        }
    }

    /// Produces the next mono output sample (already volume-scaled and
    /// clamped), advancing the read cursor. Returns silence once the
    /// buffer is exhausted or nothing is active.
    fn next_sample(&mut self) -> f32 {
        if !self.active || self.pos >= self.samples.len() {
            self.active = false;
            return 0.0;
        }
        let value = mix_sample(self.samples[self.pos], self.volume);
        let (acc, pos) = advance(self.acc, self.step, self.pos);
        self.acc = acc;
        self.pos = pos;
        if self.pos >= self.samples.len() {
            self.active = false;
        }
        value
    }
}

/// Nearest-sample rate-conversion step for one output tick: `acc`
/// accumulates by `step` each tick, and every time it crosses 1.0 the read
/// position advances (possibly by more than one, if `step > 1`). For
/// `step < 1` (output rate faster than source rate) the same source
/// sample is read on consecutive ticks — a natural repeat. For `step > 1`
/// (output rate slower than source rate) `pos` occasionally jumps by 2 in
/// one tick — a natural skip. Pure and independently testable.
fn advance(acc: f32, step: f32, pos: usize) -> (f32, usize) {
    let mut acc = acc + step;
    let mut pos = pos;
    while acc >= 1.0 {
        acc -= 1.0;
        pos += 1;
    }
    (acc, pos)
}

/// Applies volume (clamped to 0..=1, in case a caller passes something out
/// of range) and clamps the resulting sample to [-1, 1] so a loud volume
/// times a near-full-scale sample never wraps.
fn mix_sample(sample: f32, volume: f32) -> f32 {
    (sample * volume.clamp(0.0, 1.0)).clamp(-1.0, 1.0)
}

fn worker_loop(rx: mpsc::Receiver<PlayRequest>, error: Arc<Mutex<Option<String>>>) {
    let mut worker = Worker::new(error);
    loop {
        match rx.recv_timeout(IDLE_TIMEOUT) {
            Ok(req) => worker.handle(req),
            Err(RecvTimeoutError::Timeout) => worker.on_idle_timeout(),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

struct Worker {
    stream: Option<cpal::Stream>,
    /// The raw `device_name` field the current stream was opened for, so a
    /// later request naming a different device (or `None` vs `Some`)
    /// triggers a reopen. `None` here (with `stream` present) means "the
    /// default device".
    open_for: Option<String>,
    out_rate: u32,
    playback: Arc<Mutex<Playback>>,
    /// Dedup gate: log a build/resolve failure once, not every request,
    /// until a subsequent open succeeds.
    error_logged: bool,
    /// Shared with `Player`: set with a short human message on an open/build
    /// failure, cleared on the next successful open. This is the user-facing
    /// counterpart to `error_logged`'s debug-build `eprintln!`.
    error: Arc<Mutex<Option<String>>>,
}

impl Worker {
    fn new(error: Arc<Mutex<Option<String>>>) -> Self {
        Self {
            stream: None,
            open_for: None,
            out_rate: 0,
            playback: Arc::new(Mutex::new(Playback::silent())),
            error_logged: false,
            error,
        }
    }

    fn handle(&mut self, req: PlayRequest) {
        let need_reopen = self.stream.is_none() || self.open_for != req.device_name;
        if need_reopen {
            match open_stream(req.device_name.as_deref(), Arc::clone(&self.playback)) {
                Ok((stream, out_rate)) => {
                    self.stream = Some(stream);
                    self.open_for = req.device_name.clone();
                    self.out_rate = out_rate;
                    self.error_logged = false;
                    if let Ok(mut slot) = self.error.lock() {
                        *slot = None;
                    }
                }
                Err(e) => {
                    if !self.error_logged {
                        eprintln!("alert playback failed: {e}");
                        self.error_logged = true;
                    }
                    if let Ok(mut slot) = self.error.lock() {
                        *slot = Some("Couldn't open the output device".to_string());
                    }
                    self.stream = None;
                    self.open_for = None;
                    return;
                }
            }
        }

        let step = req.source_rate as f32 / self.out_rate as f32;
        if let Ok(mut pb) = self.playback.lock() {
            pb.samples = req.samples;
            pb.volume = req.volume;
            pb.step = step;
            pb.acc = 0.0;
            pb.pos = 0;
            pb.active = true;
        }
    }

    /// Called after 10s with no request. Drops the stream (closing the
    /// device) only if nothing is currently playing — a long alert sound
    /// must not be cut off by the idle timer racing its own playback.
    fn on_idle_timeout(&mut self) {
        let idle = self.playback.lock().map(|pb| !pb.active).unwrap_or(true);
        if idle {
            self.stream = None;
            self.open_for = None;
        }
    }
}

/// Resolves the named output device (falling back to the default when
/// missing or unnamed), opens its default output config, and builds a
/// stream driven by `playback`. Returns the stream (not yet playing — the
/// caller must not forget `.play()`, done here) and the device's output
/// sample rate.
fn open_stream(
    device_name: Option<&str>,
    playback: Arc<Mutex<Playback>>,
) -> anyhow::Result<(cpal::Stream, u32)> {
    let host = cpal::default_host();
    let device = resolve_device(&host, device_name)?;
    let config = device
        .default_output_config()
        .context("no default output config")?;
    let out_rate = config.sample_rate();
    let channels = config.channels() as usize;
    let err_fn = |e| eprintln!("audio output error: {e}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let pb = Arc::clone(&playback);
            device.build_output_stream(
                config.into(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    write_frames(&pb, data, channels, |v, out| *out = v);
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let pb = Arc::clone(&playback);
            device.build_output_stream(
                config.into(),
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    write_frames(&pb, data, channels, |v, out| {
                        *out = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
                    });
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let pb = Arc::clone(&playback);
            device.build_output_stream(
                config.into(),
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    write_frames(&pb, data, channels, |v, out| {
                        *out = ((v.clamp(-1.0, 1.0) + 1.0) * 32767.5) as u16;
                    });
                },
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("unsupported output format {other:?}"),
    };
    stream.play()?;
    Ok((stream, out_rate))
}

/// Shared body of the three per-format output callbacks: locks once per
/// buffer (not per frame), walks it in `channels`-wide frames pulling one
/// mono value per frame from `playback`, and writes it to every channel of
/// the frame via `write_out`. If the lock is poisoned (should never
/// happen — the worker never panics while holding it), fills silence
/// rather than propagating a panic onto the audio thread.
fn write_frames<S: Copy + Default>(
    playback: &Arc<Mutex<Playback>>,
    data: &mut [S],
    channels: usize,
    write_out: impl Fn(f32, &mut S),
) {
    match playback.lock() {
        Ok(mut pb) => {
            for frame in data.chunks_mut(channels) {
                let v = pb.next_sample();
                for out in frame {
                    write_out(v, out);
                }
            }
        }
        Err(_) => {
            for out in data.iter_mut() {
                *out = S::default();
            }
        }
    }
}

fn resolve_device(host: &cpal::Host, name: Option<&str>) -> anyhow::Result<cpal::Device> {
    if let Some(name) = name
        && let Ok(mut devices) = host.output_devices()
        && let Some(d) = devices.find(|d| d.to_string() == name)
    {
        return Ok(d);
    }
    host.default_output_device()
        .context("no default output device")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_at_unity_step_moves_one_position_per_tick_with_zero_acc() {
        let (mut acc, mut pos) = (0.0f32, 0usize);
        for expected_pos in 1..=5usize {
            (acc, pos) = advance(acc, 1.0, pos);
            assert_eq!(pos, expected_pos);
            assert!(acc.abs() < 1e-6, "acc should stay 0 at unity step, got {acc}");
        }
    }

    #[test]
    fn advance_with_step_above_one_occasionally_skips_a_position() {
        // out_rate(44100) < source_rate(48000): step > 1, so position should
        // advance by 2 on some ticks (a skipped source sample) while
        // averaging out to `step` over many ticks.
        let step = 48_000.0f32 / 44_100.0f32;
        assert!(step > 1.0);
        let (mut acc, mut pos) = (0.0f32, 0usize);
        let mut saw_a_skip = false;
        let ticks = 200;
        for _ in 0..ticks {
            let prev_pos = pos;
            (acc, pos) = advance(acc, step, pos);
            let delta = pos - prev_pos;
            assert!(delta == 1 || delta == 2, "unexpected per-tick delta {delta}");
            if delta == 2 {
                saw_a_skip = true;
            }
        }
        assert!(saw_a_skip, "expected at least one skipped source sample");
        let ratio = pos as f32 / ticks as f32;
        assert!((ratio - step).abs() < 0.01, "average advance {ratio} should track step {step}");
    }

    #[test]
    fn advance_with_half_step_repeats_each_position_twice() {
        // out_rate(96000) > source_rate(48000): step = 0.5, so each source
        // position is read on two consecutive output ticks before moving on.
        let step = 0.5f32;
        let (mut acc, mut pos) = (0.0f32, 0usize);
        let mut positions = Vec::new();
        for _ in 0..8 {
            positions.push(pos);
            (acc, pos) = advance(acc, step, pos);
        }
        assert_eq!(positions, vec![0, 0, 1, 1, 2, 2, 3, 3]);
    }

    #[test]
    fn next_sample_terminates_cleanly_at_end_of_buffer() {
        let mut pb = Playback {
            samples: Arc::new(vec![0.5, -0.5, 0.25]),
            volume: 1.0,
            step: 1.0,
            acc: 0.0,
            pos: 0,
            active: true,
        };
        assert_eq!(pb.next_sample(), 0.5);
        assert_eq!(pb.next_sample(), -0.5);
        assert_eq!(pb.next_sample(), 0.25);
        // Buffer exhausted: goes silent and stays silent, never panics.
        assert_eq!(pb.next_sample(), 0.0);
        assert!(!pb.active);
        assert_eq!(pb.next_sample(), 0.0);
    }

    #[test]
    fn mix_sample_clamps_the_sample_after_scaling() {
        assert_eq!(mix_sample(2.0, 0.5), 1.0);
        assert_eq!(mix_sample(-2.0, 0.5), -1.0);
        assert!((mix_sample(0.4, 0.5) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn mix_sample_clamps_an_out_of_range_volume_first() {
        // volume > 1 should be clamped to 1, not left to blow up the product.
        assert_eq!(mix_sample(0.5, 3.0), 0.5);
        // negative volume clamps to 0: silence, not a phase flip.
        assert_eq!(mix_sample(0.5, -1.0), 0.0);
    }

    #[test]
    fn inactive_playback_produces_silence() {
        let mut pb = Playback::silent();
        assert_eq!(pb.next_sample(), 0.0);
        assert!(!pb.active);
    }

    /// Pins the shared error slot's setter/getter contract that `Player`
    /// and `Worker` communicate through, independent of a real cpal
    /// device: starts empty, reflects a set failure, and clears again —
    /// the same sequence a real open/build failure followed by a
    /// successful reopen drives.
    #[test]
    fn shared_error_state_reads_back_what_was_set_and_clears() {
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let read = || error.lock().ok().and_then(|guard| guard.clone());

        assert_eq!(read(), None);

        *error.lock().unwrap() = Some("Couldn't open the output device".to_string());
        assert_eq!(read(), Some("Couldn't open the output device".to_string()));

        *error.lock().unwrap() = None;
        assert_eq!(read(), None);
    }
}
