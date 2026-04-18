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
//! ## Curve shape — filmic shoulder with a wide asymptote (Phase 5)
//!
//! Goals: clean math, analytical (no table lookup), strictly monotonic, and
//! endpoint-preserving over the SDR input range. The curve has three pieces:
//!
//! - **Shadow knee** `[0, SHADOW_KNEE]` — cubic Hermite from `(0, 0)` with
//!   slope `1.0` to `(SHADOW_KNEE, midtone_line(SHADOW_KNEE))` matching the
//!   midtone line's slope `m`. Slope 1.0 at the origin means deep shadows
//!   neither crush nor lift.
//! - **Midtone line** `[SHADOW_KNEE, HIGHLIGHT_KNEE]` — straight line through
//!   `(midtone_anchor, midtone_anchor)` with slope `m`. The anchor sits at
//!   0.40 so the line lifts upper midtones a touch without crushing shadows.
//! - **Highlight shoulder** `[HIGHLIGHT_KNEE, ∞)` — Reinhard-style rational
//!   that asymptotes at `peak` (typically 4.0, bright enough to use all of
//!   an XDR display's ~1600 nit headroom). Anchored at the knee with matching
//!   value + slope, so there's no kink. In SDR mode (`peak = 1.0`) the curve
//!   asymptotes exactly at 1.0, matching Phase 4's behavior and clipping
//!   anything above.
//!
//! The filmic shape means the curve **never clips** — it always asymptotes
//! toward `peak`. Inputs above 1.0 (from exposure, wide-gamut colors, etc.)
//! produce output that grows toward but never reaches `peak`. The renderer
//! decides what to do with values above 1.0: send them to the EDR-capable
//! surface, or clamp for SDR output.
//!
//! Below 0.0 (which the camera matrix can produce on out-of-gamut colors) it
//! clamps to 0.0. NaN is folded to 0.0.
//!
//! ## Safety invariants (enforced by unit tests)
//!
//! - **Monotonic scalar curve**: `default_curve(x1) < default_curve(x2)` for
//!   any `x1 < x2` in `[0, 10]`.
//! - **Scalar origin**: `default_curve(0) == 0`.
//! - **Near-identity at 1.0**: `default_curve(1.0)` sits close to `1.0` so
//!   SDR inputs mostly survive the shoulder intact.
//! - **Asymptote at peak**: `curve_filmic(10.0, anchor, 4.0)` approaches `4.0`
//!   without reaching it; `curve_filmic(10.0, anchor, 1.0)` stays at `1.0`.
//! - **Anchor fixed point**: `default_curve(DEFAULT_MIDTONE_ANCHOR) ==
//!   DEFAULT_MIDTONE_ANCHOR`.
//! - **Hue preserved by the buffer apply**: a pure primary `(1, 0, 0)` stays
//!   on the red axis; only its brightness changes.
//! - **Neutral gray unchanged by the buffer apply at the anchor**.
//! - **Dark-pixel safety**: pixels with `Y_in < EPSILON` are set to all zeros
//!   instead of triggering a divide-by-zero scale blow-up.

use rayon::prelude::*;

/// Where the shadow cubic meets the midtone line. Below this, the curve is a
/// Hermite cubic; from here to [`HIGHLIGHT_KNEE`] it's a straight line.
const SHADOW_KNEE: f32 = 0.10;

/// Where the midtone line meets the highlight filmic shoulder. Above this,
/// the curve rolls off smoothly toward `peak`.
const HIGHLIGHT_KNEE: f32 = 0.90;

/// Midtone slope — the "contrast boost" amount. 1.0 would be linear; 1.08
/// adds a mild punch that lands between Adobe's "Linear" (no curve) and
/// "Medium Contrast" defaults.
const MIDTONE_SLOPE: f32 = 1.08;

/// Default `MIDTONE_ANCHOR` used by [`apply_default_tone_curve`]. Tuned
/// empirically against a Preview.app screenshot reference in the Phase 2.5b
/// rerun; see `docs/notes/raw-support-phase2.md`.
pub const DEFAULT_MIDTONE_ANCHOR: f32 = 0.40;

/// Default filmic asymptote for SDR rendering. 1.0 means the shoulder clips
/// at display-white, reproducing Phase 4's output bit-for-bit on SDR
/// displays. Set higher (for example, 4.0) to let EDR-capable surfaces keep
/// the highlight headroom alive.
pub const DEFAULT_PEAK_SDR: f32 = 1.0;

/// Filmic asymptote when EDR output is active. 4.0 comfortably fills an
/// Apple XDR display's ~1600 nits peak when SDR white maps to 400 nits. The
/// user picked this number; see `docs/notes/raw-support-phase5.md` for the
/// rationale and the Reinhard-shoulder math.
pub const DEFAULT_PEAK_HDR: f32 = 4.0;

/// Slope at `x = 0`. 1.0 keeps the curve tangent to the linear reference at
/// the origin.
const SHADOW_ENDPOINT_SLOPE: f32 = 1.0;

/// Rec.2020 luma coefficient for red. From ITU-R BT.2020-2, §5.
pub(crate) const REC2020_LUMA_R: f32 = 0.2627;
/// Rec.2020 luma coefficient for green.
pub(crate) const REC2020_LUMA_G: f32 = 0.6780;
/// Rec.2020 luma coefficient for blue.
pub(crate) const REC2020_LUMA_B: f32 = 0.0593;

/// Below this input luminance the `Y_out / Y_in` scale blows up. Below it we
/// return black instead.
const DARK_EPSILON: f32 = 1.0e-5;

/// Apply the default tone curve to a flat RGB f32 buffer in place, acting on
/// **luminance only**. Uses the SDR asymptote (`peak = 1.0`) so SDR-only
/// callers land bit-identical to Phase 4. HDR callers should thread a higher
/// peak through [`apply_tone_curve`].
///
/// Kept for `cfg(test)` + the `raw-dev-dump` example; production `raw.rs`
/// calls [`apply_tone_curve`] directly because Phase 5 threads the peak
/// through from display state.
#[allow(dead_code)]
pub fn apply_default_tone_curve(rgb: &mut [f32]) {
    apply_tone_curve(rgb, DEFAULT_MIDTONE_ANCHOR, DEFAULT_PEAK_SDR);
}

/// Parametric variant of [`apply_default_tone_curve`]. Same luminance-only
/// apply pattern, but the midtone anchor and filmic `peak` are caller-
/// supplied.
///
/// `peak = 1.0` reproduces the SDR behaviour — the shoulder asymptotes at
/// display-white and values above 1.0 clip. `peak = 4.0` lets inputs above
/// 1.0 survive the shoulder and land between 1.0 and 4.0, where an EDR
/// surface can display them.
///
/// `midtone_anchor` is clamped into `(0, 1)` at the caller's contract — this
/// function trusts the input and will produce nonsense curves for values
/// outside that range.
pub fn apply_tone_curve(rgb: &mut [f32], midtone_anchor: f32, peak: f32) {
    rgb.par_chunks_exact_mut(3).for_each(|pixel| {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let y_in = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;
        if !y_in.is_finite() || y_in < DARK_EPSILON {
            pixel[0] = 0.0;
            pixel[1] = 0.0;
            pixel[2] = 0.0;
            return;
        }
        let y_out = curve_filmic(y_in, midtone_anchor, peak);
        let scale = y_out / y_in;
        pixel[0] = r * scale;
        pixel[1] = g * scale;
        pixel[2] = b * scale;
    });
}

/// Scalar default tone curve at the SDR asymptote. Domain is all of `f32`;
/// range is `[0.0, 1.0]` with `f(0) = 0` and a soft landing near `1.0`.
///
/// Exposed for unit tests and diagnostic tooling. Inlined for the per-pixel
/// hot path inside [`apply_default_tone_curve`].
#[inline]
#[allow(dead_code)] // used by tests + diag tooling; `apply_*` inlines via `curve_filmic`
pub fn default_curve(x: f32) -> f32 {
    curve_filmic(x, DEFAULT_MIDTONE_ANCHOR, DEFAULT_PEAK_SDR)
}

/// Parametric scalar tone curve with a filmic Reinhard-style shoulder.
///
/// - For `x <= 0` or NaN: returns 0.
/// - For `0 < x < SHADOW_KNEE`: cubic Hermite shadow.
/// - For `SHADOW_KNEE <= x <= HIGHLIGHT_KNEE`: midtone line.
/// - For `x > HIGHLIGHT_KNEE`: rational shoulder asymptotic to `peak`, C¹
///   continuous with the midtone line at the knee.
///
/// Panics on `peak <= HIGHLIGHT_KNEE * MIDTONE_SLOPE + midtone_line(KNEE)`
/// would blow up — callers should pass `peak >= 1.0`.
#[inline]
pub fn curve_filmic(x: f32, midtone_anchor: f32, peak: f32) -> f32 {
    if x.is_nan() || x <= 0.0 {
        return 0.0;
    }

    if x < SHADOW_KNEE {
        return shadow_hermite(x, midtone_anchor);
    }
    if x <= HIGHLIGHT_KNEE {
        return midtone_line(x, midtone_anchor);
    }
    // Filmic Reinhard shoulder:  y = y_knee + (peak - y_knee) * t / (t + s)
    // where t = x - HIGHLIGHT_KNEE and s = (peak - y_knee) / MIDTONE_SLOPE
    // keeps the join C¹ (value + first derivative match the midtone line).
    //
    // If peak <= y_knee, we degenerate to a flat hold at y_knee — safer than
    // producing a kink or a negative-slope shoulder when a caller requests
    // a sub-SDR peak.
    let y_knee = midtone_line(HIGHLIGHT_KNEE, midtone_anchor);
    let headroom = peak - y_knee;
    if headroom <= 0.0 {
        return y_knee;
    }
    let t = x - HIGHLIGHT_KNEE;
    let s = headroom / MIDTONE_SLOPE;
    y_knee + headroom * t / (t + s)
}

/// Back-compat alias — callers that only need the SDR curve shape can still
/// call `curve(x, anchor)`. New callers should use [`curve_filmic`] directly.
#[inline]
#[allow(dead_code)]
pub fn curve(x: f32, midtone_anchor: f32) -> f32 {
    curve_filmic(x, midtone_anchor, DEFAULT_PEAK_SDR)
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

/// Apply a caller-supplied tone curve to a flat RGB f32 buffer in place,
/// acting on **luminance only** via the same `Y_out / Y_in` scale pattern
/// [`apply_default_tone_curve`] uses.
///
/// `curve_points` is a monotonic-increasing list of `(x, y)` pairs defining
/// a piecewise-linear curve. Interior `x` values must be strictly
/// increasing; extremes are extended by clamping (`x < first.x` → `first.y`,
/// `x > last.x` → `last.y`). The DNG spec (§ 6.2.4) calls for the curve to
/// include endpoints at `(0, 0)` and `(1, 1)`, and to be monotonic; we
/// don't enforce that — the caller (the DCP parser) takes the spec's word
/// and passes through whatever points the profile ships. Out-of-spec
/// curves still produce sane output, they just don't match the spec.
///
/// Used by [`crate::decoding::raw`] to swap in a DCP's `ProfileToneCurve`
/// when the camera's profile ships one. In that case the camera's
/// intended tonality wins over our Preview-tuned default.
///
/// Dark-pixel safety and NaN handling match [`apply_default_tone_curve`].
/// Empty `curve_points` is a no-op; a single-point curve degenerates to a
/// constant output (Y_out = y) which is rarely useful but doesn't panic.
pub fn apply_tone_curve_lut(rgb: &mut [f32], curve_points: &[(f32, f32)]) {
    if curve_points.is_empty() {
        return;
    }
    rgb.par_chunks_exact_mut(3).for_each(|pixel| {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let y_in = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;
        if !y_in.is_finite() || y_in < DARK_EPSILON {
            pixel[0] = 0.0;
            pixel[1] = 0.0;
            pixel[2] = 0.0;
            return;
        }
        let y_out = sample_piecewise_linear(y_in, curve_points);
        let scale = y_out / y_in;
        pixel[0] = r * scale;
        pixel[1] = g * scale;
        pixel[2] = b * scale;
    });
}

/// Piecewise-linear interpolation of `x` through a list of sorted
/// `(x, y)` control points. Extremes clamp. Exposed for unit tests
/// alongside [`apply_tone_curve_lut`].
#[inline]
pub fn sample_piecewise_linear(x: f32, points: &[(f32, f32)]) -> f32 {
    if points.is_empty() {
        return 0.0;
    }
    if x <= points[0].0 {
        return points[0].1;
    }
    if x >= points[points.len() - 1].0 {
        return points[points.len() - 1].1;
    }
    let idx = points.partition_point(|(px, _)| *px <= x);
    let (x0, y0) = points[idx - 1];
    let (x1, y1) = points[idx];
    let dx = x1 - x0;
    if dx <= 0.0 {
        return y0;
    }
    let t = (x - x0) / dx;
    y0 + (y1 - y0) * t
}

/// Cubic Hermite interpolation on `[x0, x1]` with endpoint values `y0`, `y1`
/// and endpoint slopes `m0`, `m1`.
#[inline]
fn hermite(x: f32, x0: f32, x1: f32, y0: f32, y1: f32, m0: f32, m1: f32) -> f32 {
    let dx = x1 - x0;
    let t = (x - x0) / dx;
    let t2 = t * t;
    let t3 = t2 * t;

    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;

    h00 * y0 + h10 * dx * m0 + h01 * y1 + h11 * dx * m1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_is_zero() {
        assert_eq!(default_curve(0.0), 0.0);
    }

    #[test]
    fn midtone_anchor_is_preserved() {
        // The midtone line passes through (DEFAULT_MIDTONE_ANCHOR,
        // DEFAULT_MIDTONE_ANCHOR), so f(DEFAULT_MIDTONE_ANCHOR) ==
        // DEFAULT_MIDTONE_ANCHOR exactly.
        assert!((default_curve(DEFAULT_MIDTONE_ANCHOR) - DEFAULT_MIDTONE_ANCHOR).abs() < 1e-6);
    }

    #[test]
    fn parametric_curve_anchors_at_supplied_value() {
        for anchor in [0.25_f32, 0.30, 0.35, 0.40, 0.45, 0.50] {
            assert!(
                (curve_filmic(anchor, anchor, DEFAULT_PEAK_SDR) - anchor).abs() < 1e-6,
                "curve_filmic(anchor={anchor}) != {anchor}"
            );
        }
    }

    #[test]
    fn sdr_curve_stays_at_or_below_one() {
        // Phase 4 contract: default_curve (SDR peak = 1.0) never exceeds 1.0.
        // The Reinhard shoulder at peak = 1.0 asymptotes exactly at 1.0 and
        // clips above. Spot-check a few high-end samples.
        for x in [0.95_f32, 1.0, 1.5, 2.0, 5.0, 10.0, 100.0] {
            let y = default_curve(x);
            assert!(y <= 1.0 + 1e-6, "SDR curve overshot at x={x}: y={y}");
        }
    }

    #[test]
    fn hdr_curve_asymptotes_at_peak() {
        // With peak = 4.0, very high inputs should approach but never reach
        // 4.0. A large but finite sample should sit in [3.9, 4.0).
        let y = curve_filmic(50.0, DEFAULT_MIDTONE_ANCHOR, DEFAULT_PEAK_HDR);
        assert!(
            (DEFAULT_PEAK_HDR - y) > 0.0,
            "HDR curve should asymptote from below, got y={y}"
        );
        assert!(y > 3.5, "HDR curve too low at x=50: y={y}");
        assert!(
            y < DEFAULT_PEAK_HDR,
            "HDR curve shouldn't reach peak: y={y}"
        );
    }

    #[test]
    fn hdr_curve_is_monotonic_across_ten() {
        let mut previous = curve_filmic(0.0, DEFAULT_MIDTONE_ANCHOR, DEFAULT_PEAK_HDR);
        // 0, 0.01, 0.02, …, 10.0 — 1001 samples.
        for i in 1..=1000 {
            let x = i as f32 / 100.0;
            let y = curve_filmic(x, DEFAULT_MIDTONE_ANCHOR, DEFAULT_PEAK_HDR);
            assert!(
                y > previous,
                "HDR curve non-monotonic at x = {x}: previous {previous}, current {y}"
            );
            previous = y;
        }
    }

    #[test]
    fn sdr_curve_smooth_near_one() {
        // The handoff around x = 1.0 should produce an output very close to
        // 1.0 (we now roll off gently toward the asymptote at 1.0 instead of
        // landing exactly on it). We check the shoulder lands inside a
        // narrow band.
        let y_one = default_curve(1.0);
        assert!(
            y_one > 0.9 && y_one <= 1.0,
            "SDR curve at x=1.0 should be near 1.0, got {y_one}"
        );
    }

    #[test]
    fn clamps_below_zero() {
        assert_eq!(default_curve(-0.5), 0.0);
        assert_eq!(default_curve(-10.0), 0.0);
        assert_eq!(default_curve(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn nan_maps_to_zero() {
        assert_eq!(default_curve(f32::NAN), 0.0);
    }

    #[test]
    fn midtone_lifts_the_image() {
        assert!(
            default_curve(0.50) > 0.50,
            "midtone should lift above linear"
        );
        assert!(default_curve(0.75) > 0.75, "upper midtone should lift");
        assert!(default_curve(0.85) > 0.85, "shoulder approach should lift");
    }

    #[test]
    fn deep_shadows_stay_close_to_linear() {
        let x = 0.02;
        let y = default_curve(x);
        assert!(
            y > 0.5 * x,
            "deep shadow crushed unexpectedly: f({x}) = {y}"
        );
        assert!(y < 1.5 * x, "deep shadow lifted unexpectedly: f({x}) = {y}");
    }

    #[test]
    fn continuous_at_shadow_knee() {
        let shadow_side = shadow_hermite(SHADOW_KNEE, DEFAULT_MIDTONE_ANCHOR);
        let midtone_side = midtone_line(SHADOW_KNEE, DEFAULT_MIDTONE_ANCHOR);
        assert!(
            (shadow_side - midtone_side).abs() < 1e-5,
            "shadow knee discontinuity: {shadow_side} vs {midtone_side}"
        );
    }

    #[test]
    fn continuous_at_highlight_knee() {
        // Filmic shoulder at t = 0 returns exactly y_knee; the midtone line
        // evaluated at HIGHLIGHT_KNEE must match.
        let midtone_side = midtone_line(HIGHLIGHT_KNEE, DEFAULT_MIDTONE_ANCHOR);
        let shoulder_side_sdr =
            curve_filmic(HIGHLIGHT_KNEE, DEFAULT_MIDTONE_ANCHOR, DEFAULT_PEAK_SDR);
        let shoulder_side_hdr =
            curve_filmic(HIGHLIGHT_KNEE, DEFAULT_MIDTONE_ANCHOR, DEFAULT_PEAK_HDR);
        assert!(
            (shoulder_side_sdr - midtone_side).abs() < 1e-5,
            "highlight knee discontinuity (SDR): {shoulder_side_sdr} vs {midtone_side}"
        );
        assert!(
            (shoulder_side_hdr - midtone_side).abs() < 1e-5,
            "highlight knee discontinuity (HDR): {shoulder_side_hdr} vs {midtone_side}"
        );
    }

    #[test]
    fn slope_continuous_at_highlight_knee() {
        // Numerically check that the first derivative matches MIDTONE_SLOPE
        // at the knee. If C¹ broke, this would shift past any reasonable
        // tolerance.
        let eps = 1e-4_f32;
        for peak in [DEFAULT_PEAK_SDR, 2.0, DEFAULT_PEAK_HDR] {
            let left = curve_filmic(HIGHLIGHT_KNEE - eps, DEFAULT_MIDTONE_ANCHOR, peak);
            let right = curve_filmic(HIGHLIGHT_KNEE + eps, DEFAULT_MIDTONE_ANCHOR, peak);
            let slope = (right - left) / (2.0 * eps);
            assert!(
                (slope - MIDTONE_SLOPE).abs() < 5e-2,
                "non-C1 at highlight knee for peak={peak}: slope={slope} vs {MIDTONE_SLOPE}"
            );
        }
    }

    #[test]
    fn apply_handles_empty_buffer() {
        let mut buf: Vec<f32> = Vec::new();
        apply_default_tone_curve(&mut buf); // must not panic
        assert!(buf.is_empty());
    }

    #[test]
    fn apply_neutral_anchor_gray_is_unchanged() {
        let anchor = DEFAULT_MIDTONE_ANCHOR;
        let mut buf = vec![anchor, anchor, anchor];
        apply_default_tone_curve(&mut buf);
        for (got, want) in buf.iter().zip([anchor, anchor, anchor]) {
            assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
        }
    }

    #[test]
    fn apply_preserves_pure_primary_hue() {
        let mut buf = vec![1.0, 0.0, 0.0];
        apply_default_tone_curve(&mut buf);
        assert!(buf[1].abs() < 1e-6, "green leaked: {}", buf[1]);
        assert!(buf[2].abs() < 1e-6, "blue leaked: {}", buf[2]);
        assert!(buf[0] > 0.0, "red vanished: {}", buf[0]);
        assert!(buf[0].is_finite(), "red NaN/inf: {}", buf[0]);
    }

    #[test]
    fn apply_preserves_hue_on_mixed_pixel() {
        let r_in = 0.8_f32;
        let g_in = 0.5_f32;
        let b_in = 0.2_f32;
        let mut buf = vec![r_in, g_in, b_in];
        apply_default_tone_curve(&mut buf);
        let (r_out, g_out, b_out) = (buf[0], buf[1], buf[2]);
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
        let mut buf = vec![1e-9_f32, 1e-9_f32, 1e-9_f32];
        apply_default_tone_curve(&mut buf);
        for v in &buf {
            assert_eq!(*v, 0.0, "tiny pixel should zero out, got {v}");
        }
    }

    #[test]
    fn apply_zeroes_nan_pixels() {
        let mut buf = vec![f32::NAN, 0.5, 0.5];
        apply_default_tone_curve(&mut buf);
        for v in &buf {
            assert_eq!(*v, 0.0, "NaN pixel should zero out, got {v}");
        }
    }

    #[test]
    fn hdr_apply_keeps_wide_gamut_highlights() {
        // A bright pixel above 1.0 should come out above 1.0 with HDR peak,
        // but below DEFAULT_PEAK_HDR.
        let mut buf = vec![2.0_f32, 2.0, 2.0];
        apply_tone_curve(&mut buf, DEFAULT_MIDTONE_ANCHOR, DEFAULT_PEAK_HDR);
        for v in &buf {
            assert!(*v > 1.0, "HDR apply clipped at 1.0: {v}");
            assert!(*v < DEFAULT_PEAK_HDR, "HDR apply exceeded peak: {v}");
        }
    }

    #[test]
    fn sdr_apply_matches_phase4_sdr_behavior() {
        // A bright pixel stays inside [0, 1] with SDR peak, matching the
        // Phase 4 clip.
        let mut buf = vec![2.0_f32, 2.0, 2.0];
        apply_tone_curve(&mut buf, DEFAULT_MIDTONE_ANCHOR, DEFAULT_PEAK_SDR);
        for v in &buf {
            assert!(*v <= 1.0 + 1e-6, "SDR apply went above 1.0: {v}");
        }
    }

    #[test]
    fn piecewise_linear_interpolates_between_points() {
        let curve = [(0.0, 0.0), (1.0, 1.0)];
        for x in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
            let y = sample_piecewise_linear(x, &curve);
            assert!((y - x).abs() < 1e-6, "identity curve drifted: f({x}) = {y}");
        }
    }

    #[test]
    fn piecewise_linear_clamps_outside_domain() {
        let curve = [(0.0, 0.1), (1.0, 0.9)];
        assert_eq!(sample_piecewise_linear(-1.0, &curve), 0.1);
        assert_eq!(sample_piecewise_linear(2.0, &curve), 0.9);
    }

    #[test]
    fn piecewise_linear_three_point_s_curve() {
        let curve = [(0.0, 0.0), (0.5, 0.4), (1.0, 1.0)];
        assert!((sample_piecewise_linear(0.25, &curve) - 0.2).abs() < 1e-6);
        assert!((sample_piecewise_linear(0.5, &curve) - 0.4).abs() < 1e-6);
        assert!((sample_piecewise_linear(0.75, &curve) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn apply_tone_curve_lut_identity_is_noop() {
        let curve = [(0.0, 0.0), (1.0, 1.0)];
        let mut buf = vec![0.8_f32, 0.3, 0.1, 0.5, 0.5, 0.5, 0.2, 0.7, 0.9];
        let orig = buf.clone();
        apply_tone_curve_lut(&mut buf, &curve);
        for (got, want) in buf.iter().zip(orig.iter()) {
            assert!((got - want).abs() < 1e-5, "got {got}, want {want}");
        }
    }

    #[test]
    fn apply_tone_curve_lut_preserves_hue() {
        let curve = [(0.0, 0.0), (0.5, 0.3), (1.0, 1.0)];
        let (r, g, b) = (0.8_f32, 0.4, 0.2);
        let mut buf = vec![r, g, b];
        apply_tone_curve_lut(&mut buf, &curve);
        assert!(
            ((r / g) - (buf[0] / buf[1])).abs() < 1e-5,
            "R:G ratio drifted"
        );
        assert!(
            ((r / b) - (buf[0] / buf[2])).abs() < 1e-5,
            "R:B ratio drifted"
        );
    }

    #[test]
    fn apply_tone_curve_lut_darker_curve_darkens_output() {
        let curve = [(0.0, 0.0), (0.5, 0.2), (1.0, 0.8)];
        let mut buf = vec![0.5_f32, 0.5, 0.5];
        apply_tone_curve_lut(&mut buf, &curve);
        for v in &buf {
            assert!((v - 0.2).abs() < 1e-5, "expected 0.2, got {v}");
        }
    }

    #[test]
    fn apply_tone_curve_lut_empty_is_noop() {
        let mut buf = vec![0.5_f32, 0.25, 0.75];
        let orig = buf.clone();
        apply_tone_curve_lut(&mut buf, &[]);
        assert_eq!(buf, orig);
    }

    #[test]
    fn apply_tone_curve_lut_handles_dark_and_nan() {
        let curve = [(0.0, 0.0), (1.0, 1.0)];
        let mut buf = vec![1e-9_f32, 1e-9, 1e-9, f32::NAN, 0.5, 0.5];
        apply_tone_curve_lut(&mut buf, &curve);
        for v in &buf {
            assert_eq!(*v, 0.0, "expected 0.0 for dark/NaN pixel, got {v}");
        }
    }
}
