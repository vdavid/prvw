//! Default tone curve for RAW rendering.
//!
//! Applied after the baseline exposure lift and before the ICC transform, on
//! the linear wide-gamut buffer. Each RGB component is shaped independently.
//!
//! ## Why a tone curve
//!
//! The pre-tone linear output is technically correct but visually flat. Real
//! viewers (Preview.app, Apple Photos, Lightroom, Affinity) all apply a
//! default shaping curve on top of the sensor-linear data so midtones look
//! bright and highlights roll off instead of hard-clipping. Skipping this
//! step leaves our output reading "dull" compared with every other viewer the
//! user owns.
//!
//! ## Curve shape — Adobe-leaning shoulder via Hermite knees + lifted midtone line
//!
//! Goals: clean math, analytical (no table lookup), strictly monotonic, and
//! endpoint-preserving. The curve has three pieces:
//!
//! - **Shadow knee** `[0, SHADOW_KNEE]` — cubic Hermite from `(0, 0)` with
//!   slope `1.0` to `(SHADOW_KNEE, midtone_line(SHADOW_KNEE))` matching the
//!   midtone line's slope `m`. Slope 1.0 at the origin means deep shadows
//!   neither crush nor lift (they stay on the linear reference), then ramp
//!   smoothly up into the midtone line.
//! - **Midtone line** `[SHADOW_KNEE, HIGHLIGHT_KNEE]` — straight line through
//!   `(MIDTONE_ANCHOR, MIDTONE_ANCHOR)` with slope `m`. The anchor sits at
//!   0.25 (low quarter tones), so the line passes **above** the linear
//!   reference across the middle and upper midtones. That's the "lifts the
//!   image" part of an Adobe-neutral tone curve, combined with `m > 1` for
//!   a mild contrast punch.
//! - **Highlight shoulder** `[HIGHLIGHT_KNEE, 1]` — cubic Hermite from
//!   `(HIGHLIGHT_KNEE, midtone_line(HIGHLIGHT_KNEE))` with slope `m` to
//!   `(1, 1)` with slope `HIGHLIGHT_ENDPOINT_SLOPE`. The shoulder rolls off
//!   the `m > 1` line so values approaching 1.0 don't overshoot, and lands
//!   exactly on 1.0 with a soft asymptote instead of a hard clip.
//!
//! Above 1.0 (which exposure can produce) the curve saturates at 1.0. Below
//! 0.0 (which the camera matrix can produce on out-of-gamut colors) it
//! clamps to 0.0. NaN is folded to 0.0.
//!
//! Intuition: a Bezier-like S-curve where the central "straight" section is
//! literally a straight line, anchored low on the diagonal so the curve
//! mostly lifts rather than compresses. That keeps the shape readable and
//! the code one scalar function.
//!
//! ## Safety invariants (enforced by unit tests)
//!
//! - **Monotonic**: `f(x1) < f(x2)` for any `x1 < x2` in `[0, 1]`. No hue
//!   flips, no inverted regions.
//! - **Endpoints**: `f(0) == 0`, `f(1) == 1` exactly.
//! - **Anchor fixed point**: `f(MIDTONE_ANCHOR) == MIDTONE_ANCHOR` exactly
//!   (the midtone line passes through it).
//! - **Saturation**: `f(x) == 1` for `x >= 1`, `f(x) == 0` for `x <= 0`.
//!
//! Applied per-channel in RGB (not luminance-weighted), same as Lightroom's
//! default. This desaturates saturated colors a hair but avoids the hue
//! shifts a luma-only curve would produce on already-wide-gamut data.

use rayon::prelude::*;

/// Where the shadow cubic meets the midtone line. Below this, the curve is a
/// Hermite cubic; from here to [`HIGHLIGHT_KNEE`] it's a straight line.
const SHADOW_KNEE: f32 = 0.10;

/// Where the midtone line meets the highlight cubic. Above this, the curve
/// rolls off smoothly to 1.0.
const HIGHLIGHT_KNEE: f32 = 0.90;

/// Midtone slope — the "contrast boost" amount. 1.0 would be linear; 1.08
/// adds a mild punch that lands between Adobe's "Linear" (no curve) and
/// "Medium Contrast" defaults.
const MIDTONE_SLOPE: f32 = 1.08;

/// Where the midtone line crosses the diagonal `y = x`. Set to 0.25 (low
/// quarter tones), so that `f(x) > x` across most of the middle and upper
/// range. That's what gives the overall "lift" that matches how Preview.app
/// and Affinity render RAW files by default, rather than darkening them.
const MIDTONE_ANCHOR: f32 = 0.25;

/// Slope at `x = 0`. 1.0 keeps the curve tangent to the linear reference at
/// the origin, so deep shadows stay where they are instead of crushing
/// (slope < 1) or lifting artificially (slope > 1).
const SHADOW_ENDPOINT_SLOPE: f32 = 1.0;

/// Slope at `x = 1`. Well below the midtone slope so the highlight shoulder
/// rolls off firmly; values approaching 1.0 ease in rather than slamming
/// into a hard ceiling. 0.30 gives a noticeable roll-off without flattening
/// the top end.
const HIGHLIGHT_ENDPOINT_SLOPE: f32 = 0.30;

/// Apply the default tone curve to a flat RGB f32 buffer, in place. Each
/// component is transformed independently. Safe on buffers of any length
/// including empty and non-multiple-of-3 (the curve is purely scalar).
pub fn apply_default_tone_curve(rgb: &mut [f32]) {
    rgb.par_iter_mut().for_each(|v| *v = default_curve(*v));
}

/// Scalar default tone curve. Domain is all of `f32` (NaN passes through as
/// the clamp path kicks in); range is `[0.0, 1.0]` with exact endpoints and
/// exact midpoint.
///
/// Exposed for unit tests and diagnostic tooling. Inlined for the per-pixel
/// hot path inside [`apply_default_tone_curve`].
#[inline]
pub fn default_curve(x: f32) -> f32 {
    // Clamp first: negative values (out-of-gamut matrix outputs) saturate to
    // 0, and post-exposure values above 1.0 saturate to 1.0. The shape below
    // assumes x in [0, 1]. NaN falls through the `>` / `>=` ladder and gets
    // caught by the explicit `is_nan` check so we don't push garbage into
    // the ICC transform.
    if x.is_nan() || x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }

    if x < SHADOW_KNEE {
        shadow_hermite(x)
    } else if x > HIGHLIGHT_KNEE {
        highlight_hermite(x)
    } else {
        midtone_line(x)
    }
}

/// Midtone line through `(MIDTONE_ANCHOR, MIDTONE_ANCHOR)` with slope
/// [`MIDTONE_SLOPE`]. Written in the point-slope form so the intent reads
/// straight off the code.
#[inline]
fn midtone_line(x: f32) -> f32 {
    MIDTONE_SLOPE * (x - MIDTONE_ANCHOR) + MIDTONE_ANCHOR
}

/// Shadow region: cubic Hermite from `(0, 0)` with slope
/// [`SHADOW_ENDPOINT_SLOPE`] to `(SHADOW_KNEE, midtone_line(SHADOW_KNEE))`
/// with slope [`MIDTONE_SLOPE`]. Matches the midtone line in both value and
/// slope at the join (C¹ continuity), so there's no visible kink.
#[inline]
fn shadow_hermite(x: f32) -> f32 {
    let x0 = 0.0;
    let x1 = SHADOW_KNEE;
    let y0 = 0.0;
    let y1 = midtone_line(SHADOW_KNEE);
    let m0 = SHADOW_ENDPOINT_SLOPE;
    let m1 = MIDTONE_SLOPE;
    hermite(x, x0, x1, y0, y1, m0, m1)
}

/// Highlight region: cubic Hermite from `(HIGHLIGHT_KNEE,
/// midtone_line(HIGHLIGHT_KNEE))` with slope [`MIDTONE_SLOPE`] to `(1, 1)`
/// with slope [`HIGHLIGHT_ENDPOINT_SLOPE`]. Same C¹ join as the shadow knee.
#[inline]
fn highlight_hermite(x: f32) -> f32 {
    let x0 = HIGHLIGHT_KNEE;
    let x1 = 1.0;
    let y0 = midtone_line(HIGHLIGHT_KNEE);
    let y1 = 1.0;
    let m0 = MIDTONE_SLOPE;
    let m1 = HIGHLIGHT_ENDPOINT_SLOPE;
    hermite(x, x0, x1, y0, y1, m0, m1)
}

/// Cubic Hermite interpolation on `[x0, x1]` with endpoint values `y0`, `y1`
/// and endpoint slopes `m0`, `m1`. Evaluates to `y0` at `x0` and `y1` at
/// `x1`, with the given slopes. The canonical basis formulation, rescaled
/// from the unit interval to `[x0, x1]`.
#[inline]
fn hermite(x: f32, x0: f32, x1: f32, y0: f32, y1: f32, m0: f32, m1: f32) -> f32 {
    let dx = x1 - x0;
    let t = (x - x0) / dx;
    let t2 = t * t;
    let t3 = t2 * t;

    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0; // basis for y0
    let h10 = t3 - 2.0 * t2 + t; // basis for dx*m0
    let h01 = -2.0 * t3 + 3.0 * t2; // basis for y1
    let h11 = t3 - t2; // basis for dx*m1

    h00 * y0 + h10 * dx * m0 + h01 * y1 + h11 * dx * m1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_exact() {
        assert_eq!(default_curve(0.0), 0.0);
        assert_eq!(default_curve(1.0), 1.0);
    }

    #[test]
    fn midtone_anchor_is_preserved() {
        // The midtone line passes through (MIDTONE_ANCHOR, MIDTONE_ANCHOR),
        // so f(MIDTONE_ANCHOR) == MIDTONE_ANCHOR exactly. This anchors the
        // curve's crossing with the diagonal and keeps its lift direction
        // predictable.
        assert!((default_curve(MIDTONE_ANCHOR) - MIDTONE_ANCHOR).abs() < 1e-6);
    }

    #[test]
    fn strictly_monotonic_over_256_samples() {
        // Walk the full [0, 1] range at 256 samples and assert every step is
        // strictly greater than the previous. No plateaus, no reversals —
        // either would introduce hue flips or flat patches in the output.
        let mut previous = default_curve(0.0);
        for i in 1..=256 {
            let x = i as f32 / 256.0;
            let y = default_curve(x);
            assert!(
                y > previous,
                "non-monotonic at x = {x}: previous {previous}, current {y}"
            );
            previous = y;
        }
    }

    #[test]
    fn saturates_above_one() {
        assert_eq!(default_curve(1.0), 1.0);
        assert_eq!(default_curve(1.5), 1.0);
        assert_eq!(default_curve(10.0), 1.0);
        assert_eq!(default_curve(f32::INFINITY), 1.0);
    }

    #[test]
    fn clamps_below_zero() {
        assert_eq!(default_curve(-0.5), 0.0);
        assert_eq!(default_curve(-10.0), 0.0);
        assert_eq!(default_curve(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn nan_maps_to_zero() {
        // Explicit `is_nan` check folds NaN into the zero path. Better than
        // propagating NaN into the downstream ICC transform.
        assert_eq!(default_curve(f32::NAN), 0.0);
    }

    #[test]
    fn midtone_lifts_the_image() {
        // With the anchor at MIDTONE_ANCHOR (0.25) and slope > 1, the midtone
        // line sits above the diagonal for every x above the anchor. Check a
        // few representative points: neutral midtone (0.50), upper midtone
        // (0.75), and a highlight knee sample (0.85).
        assert!(
            default_curve(0.50) > 0.50,
            "midtone should lift above linear"
        );
        assert!(default_curve(0.75) > 0.75, "upper midtone should lift");
        assert!(default_curve(0.85) > 0.85, "shoulder approach should lift");
    }

    #[test]
    fn deep_shadows_stay_close_to_linear() {
        // Shadow endpoint slope is 1.0, so very small x values stay close to
        // their linear reference. A few gray-level worth of drift is OK
        // (the Hermite curves gently upward toward the midtone line), but
        // we shouldn't crush to black.
        let x = 0.02;
        let y = default_curve(x);
        assert!(
            y > 0.5 * x,
            "deep shadow crushed unexpectedly: f({x}) = {y}"
        );
        assert!(y < 1.5 * x, "deep shadow lifted unexpectedly: f({x}) = {y}");
    }

    #[test]
    fn highlight_rolls_off_below_line() {
        // 0.95 is inside the highlight shoulder. The midtone line extended
        // would give 1.08 * (0.95 - 0.25) + 0.25 = 1.006. The shoulder must
        // roll off *below* that (so we don't overshoot 1.0) but still
        // *above* the linear reference 0.95 (so highlights still lift).
        let y = default_curve(0.95);
        assert!(y > 0.95, "shoulder crushed highlights: {y}");
        assert!(y < 1.0, "shoulder overshot 1.0: {y}");
    }

    #[test]
    fn continuous_at_shadow_knee() {
        // The Hermite knee shares value + slope with the midtone line, so
        // numerical evaluation should agree to within float rounding.
        let shadow_side = shadow_hermite(SHADOW_KNEE);
        let midtone_side = midtone_line(SHADOW_KNEE);
        assert!(
            (shadow_side - midtone_side).abs() < 1e-5,
            "shadow knee discontinuity: {shadow_side} vs {midtone_side}"
        );
    }

    #[test]
    fn continuous_at_highlight_knee() {
        let highlight_side = highlight_hermite(HIGHLIGHT_KNEE);
        let midtone_side = midtone_line(HIGHLIGHT_KNEE);
        assert!(
            (highlight_side - midtone_side).abs() < 1e-5,
            "highlight knee discontinuity: {highlight_side} vs {midtone_side}"
        );
    }

    #[test]
    fn buffer_apply_matches_scalar() {
        // Spot-check that the parallel per-pixel apply returns the same
        // values as calling the scalar curve directly. Guards against
        // off-by-one errors in the Rayon closure.
        let inputs = [0.0_f32, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0, 1.5, -0.5];
        let mut buf = inputs.to_vec();
        apply_default_tone_curve(&mut buf);
        for (i, x) in inputs.iter().enumerate() {
            let expected = default_curve(*x);
            assert!(
                (buf[i] - expected).abs() < 1e-6,
                "buffer[{i}] = {}, scalar = {expected}",
                buf[i]
            );
        }
    }

    #[test]
    fn apply_handles_empty_buffer() {
        let mut buf: Vec<f32> = Vec::new();
        apply_default_tone_curve(&mut buf); // must not panic
        assert!(buf.is_empty());
    }
}
