//! The level-history graph: a scrolling ~30 s trace of the live mic level,
//! directly below the meter bar, colored per-segment by the state it was
//! sampled in (see `ui::meter::level_color`) so the threshold reads out of
//! the trace itself. The noise floor and ceiling are drawn behind it as
//! horizontal reference lines, mirroring the meter's own markers rotated 90°.

use std::cell::Cell;
use std::collections::VecDeque;

use iced::widget::canvas;
use iced::{Color, Point, Rectangle, Renderer, Size, Theme, border, mouse};

use crate::app::Message;
use crate::ui::meter::{level_color, norm};
use crate::ui::theme::{SURFACE, TEXT_MUTED};

/// How many (level_db, threshold_db) samples the graph holds — 30 s at the
/// existing 80 ms `Tick` cadence. Also the fixed x-scale `sample_x` maps
/// against, so the trace scrolls at a constant rate rather than stretching
/// to fill the width as it fills up.
pub const HISTORY_CAPACITY: usize = 375;

/// A level reading at/below this is the "no live data" placeholder
/// (`App::latest`'s -100.0 default while paused, no device is open, or the
/// window just opened/switched devices before the first real sample), not a
/// genuine near-floor-quiet reading — real captured audio never goes below
/// `norm`'s own -70 dB domain floor. A segment with both endpoints at/below
/// this is skipped so a paused/deviceless stretch doesn't draw a flat
/// colored line that reads as a real (very quiet) signal.
const SENTINEL_DB: f32 = -99.0;

/// Everything the history graph draws: the trailing samples (oldest at the
/// front, newest at the back) plus the static noise-floor/ceiling markers
/// from the device's calibration.
pub struct History<'a> {
    pub samples: &'a VecDeque<(f32, f32)>,
    /// Fixed x-scale (see `HISTORY_CAPACITY`) — always `HISTORY_CAPACITY` in
    /// practice; kept as a field (rather than hardcoded in `paint`) so
    /// `sample_x` stays a pure, independently testable function of it.
    pub capacity: usize,
    pub noise_floor_db: f32,
    pub ceiling_db: Option<f32>,
    /// false = the ceiling is an estimate derived from the baseline margin
    /// (drawn dashed); true = a confirmed calibration point (drawn solid).
    /// Same meaning as `ui::meter::Meter::ceiling_confirmed`.
    pub ceiling_confirmed: bool,
}

/// x position of sample index `i` (0 = oldest currently held) out of `n`
/// total samples, on a fixed `capacity`-wide scale: the newest sample
/// (`i == n - 1`) always sits at the right edge (`w`), and each older sample
/// steps one `w / (capacity - 1)` increment further left. With `n < capacity`
/// (the trace hasn't filled up yet) this leaves the trace occupying only the
/// right portion of the width, scrolling into view rather than being
/// stretched to fill it from a standing start.
pub fn sample_x(i: usize, n: usize, capacity: usize, w: f32) -> f32 {
    if n == 0 || capacity <= 1 {
        return w;
    }
    let slots = (capacity - 1) as f32;
    w - ((n - 1 - i) as f32) * w / slots
}

/// y position for a level reading: `norm`'s 0..1 mapped to the frame's
/// height and inverted (a higher level draws higher up the panel), inset by
/// `inset` on both edges so the trace/lines never touch the panel's border.
fn sample_y(level_db: f32, h: f32, inset: f32) -> f32 {
    let usable = (h - inset * 2.0).max(0.0);
    inset + (1.0 - norm(level_db)) * usable
}

/// One drawn stroke of the trace, in frame-local coordinates.
struct Segment {
    from: Point,
    to: Point,
    color: Color,
}

/// Builds the trace's line segments from adjacent sample pairs. Pure (no
/// `canvas::Frame`/renderer involved) so it's directly unit-testable — a
/// `Frame` can't be constructed outside a real renderer (see `ui::meter`'s
/// `MeterState` doc comment for the same constraint).
fn build_segments(samples: &VecDeque<(f32, f32)>, capacity: usize, w: f32, h: f32, inset: f32) -> Vec<Segment> {
    let n = samples.len();
    if n < 2 {
        return Vec::new();
    }
    let mut segments = Vec::with_capacity(n - 1);
    for i in 0..n - 1 {
        let (level_a, _threshold_a) = samples[i];
        let (level_b, threshold_b) = samples[i + 1];
        if level_a <= SENTINEL_DB && level_b <= SENTINEL_DB {
            continue;
        }
        let from = Point::new(sample_x(i, n, capacity, w), sample_y(level_a, h, inset));
        let to = Point::new(sample_x(i + 1, n, capacity, w), sample_y(level_b, h, inset));
        // Colored by the sample the segment arrives at, matching how a
        // scrolling left-to-right trace reads: the newest edge always shows
        // the freshest state.
        segments.push(Segment { from, to, color: level_color(level_b, threshold_b) });
    }
    segments
}

/// Vertical inset (px) the trace and reference lines are drawn within, so
/// nothing touches the panel's rounded border.
const INSET: f32 = 3.0;

impl History<'_> {
    fn paint(&self, frame: &mut canvas::Frame) {
        let size = frame.size();
        let w = size.width;
        let h = size.height;

        // 1. Background panel, matching the meter's.
        let background = canvas::Path::rounded_rectangle(Point::ORIGIN, size, border::radius(8.0));
        frame.fill(&background, SURFACE);

        // 2. Noise floor: a faint full-width reference line.
        let floor_y = sample_y(self.noise_floor_db, h, INSET);
        frame.fill_rectangle(Point::new(0.0, floor_y), Size::new(w, 1.0), Color { a: 0.4, ..TEXT_MUTED });

        // 3. Ceiling: solid full-width line if confirmed, dashed (three
        // short segments, same technique as `ui::meter`'s dashed ceiling
        // marker, just horizontal here) if only estimated.
        if let Some(ceiling_db) = self.ceiling_db {
            let y = sample_y(ceiling_db, h, INSET);
            if self.ceiling_confirmed {
                frame.fill_rectangle(Point::new(0.0, y), Size::new(w, 1.0), TEXT_MUTED);
            } else {
                let segment_w = w / 5.0;
                for i in 0..3 {
                    let x = segment_w * (i * 2) as f32;
                    frame.fill_rectangle(Point::new(x, y), Size::new(segment_w, 1.0), TEXT_MUTED);
                }
            }
        }

        // 4. The trace: one 2px stroke per adjacent sample pair, colored by
        // the state it was sampled in.
        for segment in build_segments(self.samples, self.capacity, w, h, INSET) {
            let path = canvas::Path::line(segment.from, segment.to);
            frame.stroke(
                &path,
                canvas::Stroke::default().with_color(Color { a: 0.9, ..segment.color }).with_width(2.0),
            );
        }
    }
}

/// Rounded-to-whole-dB snapshot used only to decide whether the geometry
/// cache needs clearing. The newest sample plus the sample count stands in
/// for "did the trace change" — every `Tick` while the window is open pushes
/// exactly one fresh sample (and, once at capacity, drops exactly one old
/// one), so a changed newest-sample/len pair means the whole scrolled trace
/// needs redrawing, same as a changed `last_key` does for the meter.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DrawKey {
    len: usize,
    newest: Option<(i32, i32)>,
    noise_floor_db: i32,
    ceiling_db: Option<i32>,
    ceiling_confirmed: bool,
}

/// Rounds a dB reading to the nearest whole dB for cache-key comparisons.
/// Rust's float-to-int cast saturates NaN/±infinity to 0 rather than
/// panicking, so a not-yet-finite threshold rounds harmlessly. Same
/// one-line helper as `ui::meter`'s private `round_db` — not shared across
/// modules since it isn't exposed there.
fn round_db(db: f32) -> i32 {
    db.round() as i32
}

impl History<'_> {
    fn draw_key(&self) -> DrawKey {
        DrawKey {
            len: self.samples.len(),
            newest: self.samples.back().map(|&(level, threshold)| (round_db(level), round_db(threshold))),
            noise_floor_db: round_db(self.noise_floor_db),
            ceiling_db: self.ceiling_db.map(round_db),
            ceiling_confirmed: self.ceiling_confirmed,
        }
    }
}

/// Canvas state: a geometry cache plus the rounded-input snapshot it was last
/// drawn with — same `Cell`-based pattern as `ui::meter::MeterState` (see its
/// doc comment for why `Cell` rather than a `&mut` field).
#[derive(Default)]
pub struct HistoryState {
    cache: canvas::Cache,
    last_key: Cell<Option<DrawKey>>,
}

impl canvas::Program<Message> for History<'_> {
    type State = HistoryState;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let key = self.draw_key();
        if state.last_key.get() != Some(key) {
            state.cache.clear();
            state.last_key.set(Some(key));
        }

        let geometry = state.cache.draw(renderer, bounds.size(), |frame| self.paint(frame));

        vec![geometry]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_sample_sits_at_the_right_edge() {
        assert_eq!(sample_x(1, 2, 375, 300.0), 300.0);
        assert_eq!(sample_x(374, 375, 375, 300.0), 300.0);
    }

    #[test]
    fn second_newest_sample_sits_one_slot_left() {
        let w = 300.0;
        let expected = w - w / 374.0;
        assert!((sample_x(0, 2, 375, w) - expected).abs() < 1e-4);
    }

    #[test]
    fn partial_history_occupies_only_the_right_portion() {
        // With n well under capacity, the oldest sample held should still be
        // well short of the left edge (x == 0.0) — the trace scrolls in
        // rather than stretching to fill the width from a standing start.
        let w = 300.0;
        let oldest_x = sample_x(0, 10, 375, w);
        assert!(oldest_x > w * 0.9, "oldest of 10/375 samples should sit near the right edge, got {oldest_x}");
    }

    #[test]
    fn sample_x_handles_degenerate_capacity_without_panic() {
        assert_eq!(sample_x(0, 0, 375, 300.0), 300.0);
        assert_eq!(sample_x(0, 1, 1, 300.0), 300.0);
    }

    #[test]
    fn single_sample_draws_no_segments() {
        let mut samples = VecDeque::new();
        samples.push_back((-40.0, -27.0));
        assert!(build_segments(&samples, 375, 300.0, 120.0, INSET).is_empty());
    }

    #[test]
    fn empty_history_draws_no_segments_without_panic() {
        let samples: VecDeque<(f32, f32)> = VecDeque::new();
        assert!(build_segments(&samples, 375, 300.0, 120.0, INSET).is_empty());
    }

    #[test]
    fn two_samples_draw_one_segment_colored_by_the_newer_one() {
        let mut samples = VecDeque::new();
        samples.push_back((-40.0, -27.0)); // green (well under threshold)
        samples.push_back((-27.0, -27.0)); // red (at threshold)
        let segments = build_segments(&samples, 375, 300.0, 120.0, INSET);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].color, crate::ui::theme::RED);
    }

    #[test]
    fn both_sentinel_endpoints_are_skipped() {
        let mut samples = VecDeque::new();
        samples.push_back((-100.0, f32::NAN));
        samples.push_back((-100.0, f32::NAN));
        samples.push_back((-40.0, f32::NAN));
        // Only the (sentinel, real) pair should draw — the (sentinel,
        // sentinel) pair is skipped.
        let segments = build_segments(&samples, 375, 300.0, 120.0, INSET);
        assert_eq!(segments.len(), 1);
    }

    #[test]
    fn genuine_quiet_reading_is_not_treated_as_sentinel() {
        // -70 dB is `norm`'s own floor (genuinely quiet audio), well above
        // SENTINEL_DB (-99.0) — must still draw.
        let mut samples = VecDeque::new();
        samples.push_back((-70.0, -27.0));
        samples.push_back((-68.0, -27.0));
        let segments = build_segments(&samples, 375, 300.0, 120.0, INSET);
        assert_eq!(segments.len(), 1);
    }

    #[test]
    fn sample_y_maps_range_like_norm_inverted() {
        // Loudest (0 dBFS) draws at the top inset; quietest (-70 dB and
        // below) draws at the bottom inset.
        assert_eq!(sample_y(0.0, 120.0, INSET), INSET);
        assert_eq!(sample_y(-70.0, 120.0, INSET), 120.0 - INSET);
        assert_eq!(sample_y(-100.0, 120.0, INSET), 120.0 - INSET);
    }
}
