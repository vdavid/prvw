//! The read progress bar under the "Loading…" overlay.
//!
//! Three rounded rects through the standard overlay pill pipeline: a dark track so the bar reads on
//! a bright photo, a thin outline, and the fill. Pure geometry, so the layout is unit-testable
//! without a GPU.

use crate::pixels::Logical;
use crate::render::text::StandalonePill;

/// Bar width in logical pixels. Wide enough to make a slow read visibly move, narrow enough to sit
/// under the "Loading…" pill without becoming the loudest thing on screen.
pub const WIDTH: f32 = 160.0;

/// Bar height in logical pixels.
pub const HEIGHT: f32 = 6.0;

/// Gap between the "Loading…" pill's bottom edge and the top of the bar.
pub const TOP_GAP: f32 = 10.0;

/// Outline thickness in logical pixels.
const OUTLINE_WIDTH: f32 = 1.0;

/// Space between the outline and the fill, so the fill never smears into the outline.
const FILL_INSET: f32 = 1.5;

/// Track fill: the same translucent black as the scan status pill, so the two read as one family.
const TRACK_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.40];

/// Outline and fill grays. Light and secondary on purpose: it's a progress hint, not a headline.
const OUTLINE_COLOR: [f32; 4] = [0.85, 0.85, 0.85, 0.45];
const FILL_COLOR: [f32; 4] = [0.85, 0.85, 0.85, 0.70];

/// Build the bar's pills. `center_x` is the window's horizontal center, `top_y` the bar's top edge,
/// both in logical pixels. `fraction` is clamped to `0.0..=1.0`.
///
/// A fill too narrow to draw is left out, so a read that has barely started shows an empty track
/// rather than a stray dot at the left edge.
#[must_use]
pub fn build(center_x: Logical<f32>, top_y: Logical<f32>, fraction: f32) -> Vec<StandalonePill> {
    let left = center_x.0 - WIDTH / 2.0;
    let radius = HEIGHT / 2.0;

    let mut pills = vec![
        StandalonePill {
            x: Logical(left),
            y: top_y,
            width: Logical(WIDTH),
            height: Logical(HEIGHT),
            corner_radius: Logical(radius),
            color: TRACK_COLOR,
            border_width: Logical(0.0),
        },
        StandalonePill {
            x: Logical(left),
            y: top_y,
            width: Logical(WIDTH),
            height: Logical(HEIGHT),
            corner_radius: Logical(radius),
            color: OUTLINE_COLOR,
            border_width: Logical(OUTLINE_WIDTH),
        },
    ];

    let inner_width = WIDTH - FILL_INSET * 2.0;
    let fill_width = inner_width * fraction.clamp(0.0, 1.0);
    if fill_width >= 1.0 {
        let inner_height = HEIGHT - FILL_INSET * 2.0;
        pills.push(StandalonePill {
            x: Logical(left + FILL_INSET),
            y: Logical(top_y.0 + FILL_INSET),
            width: Logical(fill_width),
            height: Logical(inner_height),
            corner_radius: Logical(inner_height / 2.0),
            color: FILL_COLOR,
            border_width: Logical(0.0),
        });
    }

    pills
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill_of(pills: &[StandalonePill]) -> Option<&StandalonePill> {
        pills.get(2)
    }

    #[test]
    fn the_track_is_centered_on_the_window() {
        let pills = build(Logical(400.0), Logical(300.0), 0.5);
        assert_eq!(pills[0].x.0, 400.0 - WIDTH / 2.0);
        assert_eq!(pills[0].width.0, WIDTH);
        assert_eq!(pills[0].y.0, 300.0);
        assert_eq!(pills[0].height.0, HEIGHT);
    }

    #[test]
    fn the_outline_is_the_only_bordered_rect() {
        let pills = build(Logical(400.0), Logical(300.0), 1.0);
        assert_eq!(pills[0].border_width.0, 0.0, "the track is solid");
        assert_eq!(pills[1].border_width.0, OUTLINE_WIDTH);
        assert_eq!(fill_of(&pills).unwrap().border_width.0, 0.0);
    }

    #[test]
    fn the_fill_grows_with_the_fraction() {
        let inner = WIDTH - FILL_INSET * 2.0;
        let half = build(Logical(400.0), Logical(300.0), 0.5);
        assert_eq!(fill_of(&half).unwrap().width.0, inner / 2.0);

        let full = build(Logical(400.0), Logical(300.0), 1.0);
        assert_eq!(fill_of(&full).unwrap().width.0, inner);
        assert_eq!(
            fill_of(&full).unwrap().x.0,
            400.0 - WIDTH / 2.0 + FILL_INSET,
            "the fill starts inside the outline"
        );
    }

    #[test]
    fn a_read_that_has_barely_started_draws_no_fill() {
        let pills = build(Logical(400.0), Logical(300.0), 0.0);
        assert_eq!(pills.len(), 2, "track and outline only");
        assert!(fill_of(&pills).is_none());
    }

    #[test]
    fn a_fraction_out_of_range_is_clamped() {
        let inner = WIDTH - FILL_INSET * 2.0;
        let over = build(Logical(400.0), Logical(300.0), 4.2);
        assert_eq!(fill_of(&over).unwrap().width.0, inner);

        let under = build(Logical(400.0), Logical(300.0), -1.0);
        assert_eq!(under.len(), 2);
    }
}
