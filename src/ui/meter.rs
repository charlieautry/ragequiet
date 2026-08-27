//! The meter bar: a `canvas::Program` painting the live mic level against the
//! static calibration markers (noise floor, quiet point, threshold, ceiling).
//! One horizontal bar, no waveform, no animation/gradients/blur — see
//! RAGEQUIET_SPEC.md "The bar".

use std::cell::Cell;

use iced::widget::canvas;
use iced::{Color, Point, Rectangle, Renderer, Size, Theme, border, mouse};

use crate::app::Message;
use crate::ui::theme::{GREEN, RED, SURFACE, TEXT, TEXT_MUTED, YELLOW};

/// Map dBFS to 0..=1 across the bar. -70 dB at the left edge, 0 dBFS right.
pub fn norm(db: f32) -> f32 {
    ((db + 70.0) / 70.0).clamp(0.0, 1.0)
}

/// Level fill color: red at/over the threshold, yellow within 3 dB below it,
/// green further under. The 3 dB "getting loud" band matches the engine's
/// Quiet/Warning/Loud classification boundary.
///
/// A non-finite `threshold_db` (an uncalibrated device before its first calm
/// voiced frame — `Engine::threshold_db()` returns NaN then) is treated as
/// "no threshold yet": the level reads as green rather than propagating NaN
/// into every comparison below.
pub fn level_color(level_db: f32, threshold_db: f32) -> Color {
    if !threshold_db.is_finite() {
        return GREEN;
    }
    if level_db >= threshold_db {
        RED
    } else if level_db >= threshold_db - 3.0 {
        YELLOW
    } else {
        GREEN
    }
}

fn round_db(db: f32) -> i32 {
    db.round() as i32
}

/// Everything the meter draws: the live level/threshold/peak snapshot plus
/// the static markers from the device's calibration.
pub struct Meter {
    pub level_db: f32,
    pub threshold_db: f32,
    pub peak_db: f32,
    pub noise_floor_db: f32,
    pub quiet_db: Option<f32>,
    pub ceiling_db: Option<f32>,
    /// false = the ceiling is an estimate derived from the baseline margin
    /// (drawn dashed); true = a confirmed calibration point (drawn solid).
    pub ceiling_confirmed: bool,
}

/// Rounded-to-whole-dB snapshot of every input the meter draws, used only to
/// decide whether the geometry cache needs clearing. Rounding matches the
/// spec's integer-dB display rule and means sub-dB jitter on the live level
/// doesn't force a redraw every single tick.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DrawKey {
    level_db: i32,
    threshold_db: i32,
    peak_db: i32,
    noise_floor_db: i32,
    quiet_db: Option<i32>,
    ceiling_db: Option<i32>,
    ceiling_confirmed: bool,
}

impl Meter {
    fn draw_key(&self) -> DrawKey {
        DrawKey {
            level_db: round_db(self.level_db),
            threshold_db: round_db(self.threshold_db),
            peak_db: round_db(self.peak_db),
            noise_floor_db: round_db(self.noise_floor_db),
            quiet_db: self.quiet_db.map(round_db),
            ceiling_db: self.ceiling_db.map(round_db),
            ceiling_confirmed: self.ceiling_confirmed,
        }
    }

    fn paint(&self, frame: &mut canvas::Frame) {
        let size = frame.size();
        let w = size.width;
        let h = size.height;

        // 1. Background panel.
        let background = canvas::Path::rounded_rectangle(Point::ORIGIN, size, border::radius(8.0));
        frame.fill(&background, SURFACE);

        // 2. Red-shaded region past the threshold. Skipped entirely when the
        // threshold isn't finite yet (uncalibrated device, no calm voiced
        // frame observed) — `norm` clamps but doesn't cure NaN, and an
        // unclamped NaN coordinate would otherwise reach the renderer.
        if self.threshold_db.is_finite() {
            let threshold_x = (norm(self.threshold_db) * w).clamp(0.0, w);
            if threshold_x < w {
                frame.fill_rectangle(
                    Point::new(threshold_x, 0.0),
                    Size::new(w - threshold_x, h),
                    Color { a: 0.15, ..RED },
                );
            }
        }

        // 3. Current-level fill from the left, rounded on the left edge only,
        // inset vertically so it reads as a bar within the panel.
        let level_db = round_db(self.level_db) as f32;
        let level_x = (norm(level_db) * w).clamp(0.0, w);
        if level_x > 0.0 {
            const INSET: f32 = 4.0;
            let fill_color = level_color(level_db, self.threshold_db);
            let level_path = canvas::Path::rounded_rectangle(
                Point::new(0.0, INSET),
                Size::new(level_x, (h - INSET * 2.0).max(0.0)),
                border::left(4.0),
            );
            frame.fill(&level_path, Color { a: 0.85, ..fill_color });
        }

        // 4. Ticks: noise floor, calibrated quiet point, ghost peak.
        let tick_h = h * 0.6;
        let tick_y = (h - tick_h) / 2.0;
        const TICK_W: f32 = 2.0;

        draw_marker(frame, self.noise_floor_db, tick_y, tick_h, TICK_W, w, Color { a: 0.4, ..TEXT_MUTED });

        if let Some(quiet_db) = self.quiet_db {
            draw_marker(frame, quiet_db, tick_y, tick_h, TICK_W, w, TEXT_MUTED);
        }

        let peak_db_rounded = round_db(self.peak_db);
        if peak_db_rounded > round_db(self.noise_floor_db) {
            let peak_db = peak_db_rounded as f32;
            let peak_color = level_color(peak_db, self.threshold_db);
            draw_marker(frame, peak_db, tick_y, tick_h, TICK_W, w, Color { a: 0.35, ..peak_color });
        }

        // 5. Threshold: full-height line. Skipped when not finite, same
        // reasoning as the red region above.
        if self.threshold_db.is_finite() {
            draw_marker(frame, self.threshold_db, 0.0, h, TICK_W, w, Color { a: 0.9, ..TEXT });
        }

        // 6. Ceiling: solid full-height line if confirmed, dashed (three
        // short segments — canvas has no native dashed-rect primitive) if
        // only estimated from the baseline margin.
        if let Some(ceiling_db) = self.ceiling_db {
            let x = (norm(ceiling_db) * w).clamp(0.0, (w - TICK_W).max(0.0));
            if self.ceiling_confirmed {
                frame.fill_rectangle(Point::new(x, 0.0), Size::new(TICK_W, h), TEXT_MUTED);
            } else {
                let segment_h = h / 5.0;
                for i in 0..3 {
                    let y = segment_h * (i * 2) as f32;
                    frame.fill_rectangle(Point::new(x, y), Size::new(TICK_W, segment_h), TEXT_MUTED);
                }
            }
        }
    }
}

/// Draws a vertical marker (tick or full-height line) at `db`'s x position.
fn draw_marker(frame: &mut canvas::Frame, db: f32, y: f32, height: f32, width: f32, bar_width: f32, color: Color) {
    let x = (norm(db) * bar_width).clamp(0.0, (bar_width - width).max(0.0));
    frame.fill_rectangle(Point::new(x, y), Size::new(width, height), color);
}

/// Canvas state: a geometry cache plus the rounded-input snapshot it was last
/// drawn with.
///
/// `canvas::Program::draw` only ever receives `&State` (never `&mut`), so
/// there is no way to compare-and-store the previous inputs through an
/// ordinary field. `canvas::Cache` itself is built the same way — its
/// `clear`/`draw` methods take `&self` and mutate through a `RefCell`
/// internally — so `Cell<Option<DrawKey>>` follows the same pattern here
/// rather than fighting the trait for `&mut` access it doesn't offer.
/// `Meter` is rebuilt fresh from `App` state on every `view()` call, so this
/// state exists purely to avoid re-tessellating geometry when nothing the
/// bar draws has actually changed since the last frame.
#[derive(Default)]
pub struct MeterState {
    cache: canvas::Cache,
    last_key: Cell<Option<DrawKey>>,
}

impl canvas::Program<Message> for Meter {
    type State = MeterState;

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
    fn norm_maps_range() {
        assert_eq!(norm(-70.0), 0.0);
        assert_eq!(norm(0.0), 1.0);
        assert_eq!(norm(-35.0), 0.5);
        assert_eq!(norm(-100.0), 0.0);
        assert_eq!(norm(10.0), 1.0);
    }

    #[test]
    fn level_color_bands() {
        assert_eq!(level_color(-40.0, -27.0), GREEN);
        assert_eq!(level_color(-29.0, -27.0), YELLOW); // within 3 dB below
        assert_eq!(level_color(-27.0, -27.0), RED);
        assert_eq!(level_color(-10.0, -27.0), RED);
    }

    #[test]
    fn level_color_treats_non_finite_threshold_as_no_threshold() {
        // An uncalibrated device before its first calm voiced frame reports
        // threshold_db = NaN (Engine::threshold_db()); the meter must not
        // let that propagate into a red/yellow reading.
        assert_eq!(level_color(-40.0, f32::NAN), GREEN);
        assert_eq!(level_color(0.0, f32::NAN), GREEN);
        assert_eq!(level_color(-40.0, f32::INFINITY), GREEN);
    }

    #[test]
    fn non_finite_threshold_is_representable_without_panic() {
        // Regression test for the NaN-threshold case: computing the draw key
        // and painting must not panic (paint() needs a renderer, so this
        // exercises draw_key() plus the same is_finite() guards paint() uses
        // directly, since a full Frame isn't constructible in a unit test).
        let meter = Meter {
            level_db: -40.0,
            threshold_db: f32::NAN,
            peak_db: -35.0,
            noise_floor_db: -55.0,
            quiet_db: None,
            ceiling_db: None,
            ceiling_confirmed: false,
        };
        let key = meter.draw_key();
        // Rust's float-to-int cast saturates NaN to 0 rather than panicking.
        assert_eq!(key.threshold_db, 0);
        assert!(!meter.threshold_db.is_finite());
    }

    #[test]
    fn draw_key_ignores_sub_db_jitter() {
        let base = Meter {
            level_db: -30.2,
            threshold_db: -20.0,
            peak_db: -25.0,
            noise_floor_db: -50.0,
            quiet_db: Some(-45.0),
            ceiling_db: Some(-10.0),
            ceiling_confirmed: true,
        };
        let jittered = Meter { level_db: -30.4, ..base };
        assert_eq!(base.draw_key(), jittered.draw_key());

        let moved = Meter { level_db: -29.0, ..jittered };
        assert_ne!(base.draw_key(), moved.draw_key());
    }
}
