//! Default tone curve for RAW rendering.
//!
//! Applied after the baseline exposure lift and before the saturation boost
//! and the ICC transform, on the linear wide-gamut buffer. The curve is
//! applied to **luminance only**, then every RGB channel is scaled by the
//! same per-pixel `Y_out / Y_in` ratio so the pixel's chroma is preserved.
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
//! ## Why luminance-only (Phase 2.5a)
//!
//! Earlier in Phase 2 the curve ran per-channel. That's mathematically simple
//! but desaturates colors near the highlight shoulder: a pixel like
//! `(R=1.0, G=0.9, B=0.6)` has each channel compressed independently, which
//! brings the three channels closer together and drops the chroma. The hue
//! also wobbles because the three shoulders land at slightly different
//! brightnesses.
//!
//! Running the curve on luminance only, then scaling the original RGB by
//! `Y_out / Y_in`, preserves the `R:G:B` ratios exactly. Hue is untouched and
//! saturation (distance from neutral in linear RGB) scales with brightness
//! rather than collapsing near the shoulder.
//!
//! Luma weights are the **Rec.2020** coefficients (`0.2627 R + 0.6780 G +
//! 0.0593 B`) because the buffer is in linear Rec.2020 at this point in the
//! pipeline.
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
//!   `(midtone_anchor, midtone_anchor)` with slope `m`. The anchor sits at
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
//! ## Safety invariants (enforced by unit tests)
//!
//! - **Monotonic scalar curve**: `default_curve(x1) < default_curve(x2)` for
//!   any `x1 < x2` in `[0, 1]`.
//! - **Scalar endpoints**: `default_curve(0) == 0`, `default_curve(1) == 1`.
//! - **Anchor fixed point**: `default_curve(DEFAULT_MIDTONE_ANCHOR) ==
//!   DEFAULT_MIDTONE_ANCHOR`.
//! - **Hue preserved by the buffer apply**: a pure primary `(1, 0, 0)` stays
//!   on the red axis; only its brightness changes.
//! - **Neutral gray unchanged by the buffer apply at the anchor**: an
//!   `(0.25, 0.25, 0.25)` pixel comes out unchanged (Y_in == Y_out == 0.25,
//!   so scale == 1).
//! - **Dark-pixel safety**: pixels with `Y_in < EPSILON` are set to all zeros
//!   instead of triggering a divide-by-zero scale blow-up.

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

/// Default `MIDTONE_ANCHOR` used by [`apply_default_tone_curve`]. The midtone
/// line crosses the diagonal `y = x` at this x value. Set in the low midtone
/// range so the curve still lifts shadows and upper midtones (both sit above
/// the diagonal) while crossing below the upper-midtone range where
/// Preview.app stops lifting. Tuned empirically against a Preview.app
/// screenshot reference in the Phase 2.5b rerun; see
/// `docs/notes/raw-support-phase2.md`. The earlier `sips`-tuned value was
/// 0.25 — too bright, read as "washed out" against Preview on XDR displays.
pub const DEFAULT_MIDTONE_ANCHOR: f32 = 0.40;

/// Slope at `x = 0`. 1.0 keeps the curve tangent to the linear reference at
/// the origin, so deep shadows stay where they are instead of crushing
/// (slope < 1) or lifting artificially (slope > 1).
const SHADOW_ENDPOINT_SLOPE: f32 = 1.0;

/// Slope at `x = 1`. Well below the midtone slope so the highlight shoulder
/// rolls off firmly; values approaching 1.0 ease in rather than slamming
/// into a hard ceiling. 0.30 gives a noticeable roll-off without flattening
/// the top end.
const HIGHLIGHT_ENDPOINT_SLOPE: f32 = 0.30;

/// Rec.2020 luma coefficient for red. From ITU-R BT.2020-2, §5.
pub(crate) const REC2020_LUMA_R: f32 = 0.2627;
/// Rec.2020 luma coefficient for green.
pub(crate) const REC2020_LUMA_G: f32 = 0.6780;
/// Rec.2020 luma coefficient for blue.
pub(crate) const REC2020_LUMA_B: f32 = 0.0593;

/// Below this input luminance the `Y_out / Y_in` scale blows up. Below it we
/// return black instead. One 8-bit gray level is ~3.9e-3, so 1e-5 is well
/// under a gray level's worth of signal; we can't see the difference.
const DARK_EPSILON: f32 = 1.0e-5;

/// Apply the default tone curve to a flat RGB f32 buffer in place, acting on
/// **luminance only**. Each pixel's RGB is scaled uniformly by
/// `default_curve(Y_in) / Y_in`, so hue and chroma are preserved and only
/// brightness reshapes.
///
/// Layout is `[R0, G0, B0, R1, G1, B1, …]`. The buffer length must be a
/// multiple of 3. Pixels with luminance below [`DARK_EPSILON`] are set to
/// all zeros to avoid divide-by-zero blow-ups.
pub fn apply_default_tone_curve(rgb: &mut [f32]) {
    apply_tone_curve(rgb, DEFAULT_MIDTONE_ANCHOR);
}

/// Parametric variant of [`apply_default_tone_curve`]. Same luminance-only
/// apply pattern, but the midtone anchor is caller-supplied. Used by the
/// empirical parameter tuner in `examples/raw-tune.rs` to sweep across a
/// grid of candidate values; production code stays on
/// [`apply_default_tone_curve`] with [`DEFAULT_MIDTONE_ANCHOR`].
///
/// `midtone_anchor` is clamped into `(0, 1)` at the caller's contract — this
/// function trusts the input and will produce nonsense curves for values
/// outside that range.
pub fn apply_tone_curve(rgb: &mut [f32], midtone_anchor: f32) {
    rgb.par_chunks_exact_mut(3).for_each(|pixel| {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let y_in = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;
        // Guard against both NaN (propagated from the camera matrix on weird
        // inputs) and near-zero luminance (where the scale factor would
        // explode). Black out rather than poison the ICC transform.
        if !y_in.is_finite() || y_in < DARK_EPSILON {
            pixel[0] = 0.0;
            pixel[1] = 0.0;
            pixel[2] = 0.0;
            return;
        }
        let y_out = curve(y_in, midtone_anchor);
        let scale = y_out / y_in;
        pixel[0] = r * scale;
        pixel[1] = g * scale;
        pixel[2] = b * scale;
    });
}

/// Scalar default tone curve. Domain is all of `f32` (NaN falls through the
/// clamp path); range is `[0.0, 1.0]` with exact endpoints and exact
/// midpoint.
///
/// Exposed for unit tests and diagnostic tooling. Inlined for the per-pixel
/// hot path inside [`apply_default_tone_curve`]; [`apply_tone_curve`] goes
/// straight through [`curve`] for the same reason.
#[inline]
#[allow(dead_code)] // used by tests + diag tooling; `apply_*` inlines via `curve`
pub fn default_curve(x: f32) -> f32 {
    curve(x, DEFAULT_MIDTONE_ANCHOR)
}

/// Parametric scalar tone curve. Same shape as [`default_curve`], but the
/// midtone anchor is caller-supplied so the tuner can sweep it.
#[inline]
pub fn curve(x: f32, midtone_anchor: f32) -> f32 {
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
        shadow_hermite(x, midtone_anchor)
    } else if x > HIGHLIGHT_KNEE {
        highlight_hermite(x, midtone_anchor)
    } else {
        midtone_line(x, midtone_anchor)
    }
}

/// Midtone line through `(midtone_anchor, midtone_anchor)` with slope
/// [`MIDTONE_SLOPE`]. Written in the point-slope form so the intent reads
/// straight off the code.
#[inline]
fn midtone_line(x: f32, midtone_anchor: f32) -> f32 {
    MIDTONE_SLOPE * (x - midtone_anchor) + midtone_anchor
}

/// Shadow region: cubic Hermite from `(0, 0)` with slope
/// [`SHADOW_ENDPOINT_SLOPE`] to `(SHADOW_KNEE, midtone_line(SHADOW_KNEE))`
/// with slope [`MIDTONE_SLOPE`]. Matches the midtone line in both value and
/// slope at the join (C¹ continuity), so there's no visible kink.
#[inline]
fn shadow_hermite(x: f32, midtone_anchor: f32) -> f32 {
    let x0 = 0.0;
    let x1 = SHADOW_KNEE;
    let y0 = 0.0;
    let y1 = midtone_line(SHADOW_KNEE, midtone_anchor);
    let m0 = SHADOW_ENDPOINT_SLOPE;
    let m1 = MIDTONE_SLOPE;
    hermite(x, x0, x1, y0, y1, m0, m1)
}

/// Highlight region: cubic Hermite from `(HIGHLIGHT_KNEE,
/// midtone_line(HIGHLIGHT_KNEE))` with slope [`MIDTONE_SLOPE`] to `(1, 1)`
/// with slope [`HIGHLIGHT_ENDPOINT_SLOPE`]. Same C¹ join as the shadow knee.
#[inline]
fn highlight_hermite(x: f32, midtone_anchor: f32) -> f32 {
    let x0 = HIGHLIGHT_KNEE;
    let x1 = 1.0;
    let y0 = midtone_line(HIGHLIGHT_KNEE, midtone_anchor);
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
        // The midtone line passes through (DEFAULT_MIDTONE_ANCHOR,
        // DEFAULT_MIDTONE_ANCHOR), so f(DEFAULT_MIDTONE_ANCHOR) ==
        // DEFAULT_MIDTONE_ANCHOR exactly. This anchors the curve's crossing
        // with the diagonal and keeps its lift direction predictable.
        assert!((default_curve(DEFAULT_MIDTONE_ANCHOR) - DEFAULT_MIDTONE_ANCHOR).abs() < 1e-6);
    }

    #[test]
    fn parametric_curve_anchors_at_supplied_value() {
        // Sweep a few candidate anchors and check the curve's fixed point
        // lands on the anchor for each.
        for anchor in [0.25_f32, 0.30, 0.35, 0.40, 0.45, 0.50] {
            assert!(
                (curve(anchor, anchor) - anchor).abs() < 1e-6,
                "curve(anchor={anchor}, anchor={anchor}) != {anchor}"
            );
        }
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
        // With the anchor at DEFAULT_MIDTONE_ANCHOR and slope > 1, the midtone
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
        let shadow_side = shadow_hermite(SHADOW_KNEE, DEFAULT_MIDTONE_ANCHOR);
        let midtone_side = midtone_line(SHADOW_KNEE, DEFAULT_MIDTONE_ANCHOR);
        assert!(
            (shadow_side - midtone_side).abs() < 1e-5,
            "shadow knee discontinuity: {shadow_side} vs {midtone_side}"
        );
    }

    #[test]
    fn continuous_at_highlight_knee() {
        let highlight_side = highlight_hermite(HIGHLIGHT_KNEE, DEFAULT_MIDTONE_ANCHOR);
        let midtone_side = midtone_line(HIGHLIGHT_KNEE, DEFAULT_MIDTONE_ANCHOR);
        assert!(
            (highlight_side - midtone_side).abs() < 1e-5,
            "highlight knee discontinuity: {highlight_side} vs {midtone_side}"
        );
    }

    #[test]
    fn apply_handles_empty_buffer() {
        let mut buf: Vec<f32> = Vec::new();
        apply_default_tone_curve(&mut buf); // must not panic
        assert!(buf.is_empty());
    }

    #[test]
    fn apply_neutral_anchor_gray_is_unchanged() {
        // A neutral pixel at the midtone anchor has Y_in == Y_out == anchor,
        // so the scale factor is exactly 1. Output equals input to within
        // float rounding.
        let anchor = DEFAULT_MIDTONE_ANCHOR;
        let mut buf = vec![anchor, anchor, anchor];
        apply_default_tone_curve(&mut buf);
        for (got, want) in buf.iter().zip([anchor, anchor, anchor]) {
            assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
        }
    }

    #[test]
    fn apply_preserves_pure_primary_hue() {
        // A pure red pixel (1, 0, 0) should stay on the red axis: green and
        // blue stay at exactly 0 (their input was 0, and the scale factor
        // multiplies 0 by anything to get 0). Only red changes brightness.
        let mut buf = vec![1.0, 0.0, 0.0];
        apply_default_tone_curve(&mut buf);
        assert!(buf[1].abs() < 1e-6, "green leaked: {}", buf[1]);
        assert!(buf[2].abs() < 1e-6, "blue leaked: {}", buf[2]);
        // Red itself gets scaled by default_curve(Y_red) / Y_red where
        // Y_red = REC2020_LUMA_R. Just check it's bounded and nonzero.
        assert!(buf[0] > 0.0, "red vanished: {}", buf[0]);
        assert!(buf[0].is_finite(), "red NaN/inf: {}", buf[0]);
    }

    #[test]
    fn apply_preserves_hue_on_mixed_pixel() {
        // Pick a moderately bright colored pixel. The ratio R:G:B must stay
        // the same before and after the tone curve, since every channel
        // gets multiplied by the same scale factor.
        let r_in = 0.8_f32;
        let g_in = 0.5_f32;
        let b_in = 0.2_f32;
        let mut buf = vec![r_in, g_in, b_in];
        apply_default_tone_curve(&mut buf);
        let (r_out, g_out, b_out) = (buf[0], buf[1], buf[2]);
        // Same ratio check: r_out / g_out == r_in / g_in.
        let ratio_rg_in = r_in / g_in;
        let ratio_rg_out = r_out / g_out;
        assert!(
            (ratio_rg_in - ratio_rg_out).abs() < 1e-5,
            "R:G ratio drifted: in={ratio_rg_in}, out={ratio_rg_out}"
        );
        let ratio_rb_in = r_in / b_in;
        let ratio_rb_out = r_out / b_out;
        assert!(
            (ratio_rb_in - ratio_rb_out).abs() < 1e-5,
            "R:B ratio drifted: in={ratio_rb_in}, out={ratio_rb_out}"
        );
    }

    #[test]
    fn apply_zeroes_near_black_pixels() {
        // Input luminance below DARK_EPSILON: output is all-zero rather than
        // a divide-by-zero blow-up.
        let mut buf = vec![1e-9_f32, 1e-9_f32, 1e-9_f32];
        apply_default_tone_curve(&mut buf);
        for v in &buf {
            assert_eq!(*v, 0.0, "tiny pixel should zero out, got {v}");
        }
    }

    #[test]
    fn apply_zeroes_nan_pixels() {
        // If any RGB component is NaN, the luminance is NaN, so we zero out.
        // Guards against the camera matrix emitting NaN on pathological
        // inputs and poisoning the ICC transform.
        let mut buf = vec![f32::NAN, 0.5, 0.5];
        apply_default_tone_curve(&mut buf);
        for v in &buf {
            assert_eq!(*v, 0.0, "NaN pixel should zero out, got {v}");
        }
    }
}
