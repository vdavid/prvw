//! Highlight recovery for RAW rendering.
//!
//! When a sensor pixel's brightest channel clips (bright sky, specular
//! highlight, a window in a silhouette shot, etc.), we desaturate the pixel
//! toward its luminance. This keeps the hue from skewing (the classic
//! symptom: a bright white cloud drifts magenta or cyan because one channel
//! clips while the other two keep rising) and preserves perceived brightness.
//! The downstream tone curve then compresses the now-near-neutral highlight
//! to near-white without a hue shift.
//!
//! ## Where in the pipeline
//!
//! Applied in linear Rec.2020, AFTER the baseline exposure lift and BEFORE
//! the tone curve. Exposure can push in-gamut values above 1.0 into recovery
//! territory, so running after exposure addresses both native sensor
//! clipping and exposure-induced overflow. Running before the tone curve
//! means the curve still sees a monotonic, hue-preserving input.
//!
//! ## Algorithm — desaturate to luminance via smoothstep
//!
//! For every pixel (R, G, B) in linear Rec.2020:
//!
//! ```text
//! m = max(R, G, B)
//! if m <= threshold: no change
//! else:
//!   t = smoothstep(threshold, ceiling, m)
//!   Y = luma(R, G, B)                        // Rec.2020 weights
//!   (R, G, B) = mix((R, G, B), (Y, Y, Y), t)
//! ```
//!
//! `smoothstep(a, b, x) = 3s² − 2s³` where `s = clamp((x−a)/(b−a), 0, 1)`.
//!
//! Between `threshold` and `ceiling` the pixel softly desaturates toward its
//! luminance. Above `ceiling` it's pure gray at luminance Y (still above 1.0
//! if exposure pushed it there — the tone curve handles the compression to
//! near-white). Below `threshold` the pixel passes through untouched.
//!
//! We don't clamp to 1.0: keeping the over-ceiling value alive lets the tone
//! curve shape the shoulder the same way it does for normal highlights.
//!
//! ## Why desaturate-to-luminance instead of rebuild
//!
//! dcraw's "rebuild" modes reconstruct clipped channels from the unclipped
//! ones, which can recover actual color detail but risks colored artifacts
//! at clip boundaries. For a viewer, blend-to-neutral is reliable, has no
//! artifacts, preserves hue direction (no inversion), and produces the
//! "natural" look photographers expect: bright highlights drift toward
//! white, not magenta.
//!
//! ## Safety invariants (enforced by unit tests)
//!
//! - **In-gamut pass-through**: `max(R, G, B) <= threshold` is a no-op.
//! - **Smooth join at threshold**: at `max == threshold`, output equals
//!   input (smoothstep evaluates to 0).
//! - **Full desaturation at ceiling**: at `max == ceiling`, output equals
//!   `(Y, Y, Y)` (smoothstep evaluates to 1).
//! - **Hue direction preserved**: during partial recovery, no channel
//!   crosses another (no R:G:B inversion).
//! - **Neutral is unchanged**: `(v, v, v)` stays `(v, v, v)` regardless of
//!   `v`.
//! - **Monotonic progression**: scanning `max` from `threshold` to
//!   `ceiling`, the output moves monotonically from input toward
//!   `(Y, Y, Y)`.
//! - **Malformed parameters**: if `threshold >= ceiling`, we treat the
//!   transition as a hard step at `threshold` — clipped pixels land on
//!   `(Y, Y, Y)` immediately. The viewer keeps rendering; no panic.

use rayon::prelude::*;

use super::tone_curve::{REC2020_LUMA_B, REC2020_LUMA_G, REC2020_LUMA_R};

/// Default value above which we start desaturating. `0.95` sits just under
/// the sensor clip point, catching pixels that are about to lose a channel
/// while leaving everything safely in-gamut alone. Close to the value
/// dcraw's blend mode uses by default.
pub const DEFAULT_THRESHOLD: f32 = 0.95;

/// Default value at which we finish desaturating. `1.20` gives a ~0.25-wide
/// transition. The `+0.25` headroom above `1.0` covers the range that the
/// baseline-exposure lift (+0.5 EV default, 2 EV clamp) can plausibly push
/// a near-clip pixel into. Beyond `ceiling` the pixel is pure gray at `Y`
/// and the tone curve shapes the final roll-off.
pub const DEFAULT_CEILING: f32 = 1.20;

/// Apply highlight recovery with the default threshold and ceiling.
///
/// Layout is `[R0, G0, B0, R1, G1, B1, …]`; length must be a multiple of 3.
pub fn apply_default_highlight_recovery(rgb: &mut [f32]) {
    apply_highlight_recovery(rgb, DEFAULT_THRESHOLD, DEFAULT_CEILING);
}

/// Parametric variant of [`apply_default_highlight_recovery`]. Exposed so
/// the empirical tuner and future Phase 3.3 DCP code can override the
/// threshold and ceiling per camera.
///
/// `threshold < ceiling` is the well-formed regime; if a caller passes
/// `threshold >= ceiling` we fall back to a hard step at `threshold` rather
/// than dividing by zero. Non-finite inputs are left untouched.
pub fn apply_highlight_recovery(rgb: &mut [f32], threshold: f32, ceiling: f32) {
    rgb.par_chunks_exact_mut(3).for_each(|pixel| {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let m = r.max(g).max(b);
        if !m.is_finite() || m <= threshold {
            return;
        }
        let t = smoothstep(threshold, ceiling, m);
        let y = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;
        pixel[0] = r + (y - r) * t;
        pixel[1] = g + (y - g) * t;
        pixel[2] = b + (y - b) * t;
    });
}

/// Classic Hermite smoothstep. Degenerate domain (`b <= a`, including NaN
/// on either end) collapses to a hard step at `a`: any `x > a` gets `1`,
/// anything else gets `0`. That's the "malformed parameters" fallback
/// mentioned in the module doc.
#[inline]
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    // `partial_cmp` returns `None` on NaN, which we treat the same as the
    // degenerate-range case. Only `Greater` means the domain is well
    // formed.
    if b.partial_cmp(&a) != Some(std::cmp::Ordering::Greater) {
        return if x > a { 1.0 } else { 0.0 };
    }
    let s = ((x - a) / (b - a)).clamp(0.0, 1.0);
    s * s * (3.0 - 2.0 * s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rec.2020 luminance of an (r, g, b) triple. Mirrors what the apply
    /// function uses internally; duplicated here so test expectations stay
    /// readable.
    fn luma(r: f32, g: f32, b: f32) -> f32 {
        REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b
    }

    #[test]
    fn empty_buffer_does_not_panic() {
        let mut buf: Vec<f32> = Vec::new();
        apply_default_highlight_recovery(&mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn in_gamut_pixel_is_untouched() {
        // (0.5, 0.4, 0.3) has max = 0.5 < DEFAULT_THRESHOLD, so the pixel
        // passes through bit-identical.
        let mut buf = vec![0.5_f32, 0.4, 0.3];
        let expected = buf.clone();
        apply_default_highlight_recovery(&mut buf);
        for (got, want) in buf.iter().zip(expected.iter()) {
            assert!(
                (got - want).abs() < 1e-6,
                "pass-through drifted: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn already_neutral_highlight_is_unchanged() {
        // (1.5, 1.5, 1.5): max = 1.5 > ceiling, so we'd fully desaturate.
        // But the pixel is already neutral — luma equals each channel —
        // so the mix toward (Y, Y, Y) is a no-op.
        let mut buf = vec![1.5_f32, 1.5, 1.5];
        apply_default_highlight_recovery(&mut buf);
        for v in &buf {
            assert!((v - 1.5).abs() < 1e-5, "neutral drifted: {v}");
        }
    }

    #[test]
    fn partial_recovery_matches_formula() {
        // (1.2, 0.3, 0.1): R clipped, G and B well inside. Compute the
        // expected output from the spec formula and compare to the module.
        let (r, g, b) = (1.2_f32, 0.3, 0.1);
        let threshold = DEFAULT_THRESHOLD;
        let ceiling = DEFAULT_CEILING;
        let m = r.max(g).max(b); // 1.2
        let s_raw = (m - threshold) / (ceiling - threshold);
        let s = s_raw.clamp(0.0, 1.0);
        let t = s * s * (3.0 - 2.0 * s);
        let y = luma(r, g, b);
        let want = [r + (y - r) * t, g + (y - g) * t, b + (y - b) * t];

        let mut buf = vec![r, g, b];
        apply_highlight_recovery(&mut buf, threshold, ceiling);
        for (i, (got, exp)) in buf.iter().zip(want.iter()).enumerate() {
            assert!(
                (got - exp).abs() < 1e-5,
                "channel {i}: got {got}, want {exp}"
            );
        }
    }

    #[test]
    fn threshold_is_smooth_start() {
        // At `max == threshold`, smoothstep evaluates to 0, so the output
        // equals the input. Pick a pixel whose max sits exactly on the
        // threshold.
        let (threshold, ceiling) = (0.95_f32, 1.20);
        let (r, g, b) = (0.95_f32, 0.5, 0.2);
        let mut buf = vec![r, g, b];
        apply_highlight_recovery(&mut buf, threshold, ceiling);
        assert!(
            (buf[0] - r).abs() < 1e-6,
            "R drifted at threshold: {}",
            buf[0]
        );
        assert!(
            (buf[1] - g).abs() < 1e-6,
            "G drifted at threshold: {}",
            buf[1]
        );
        assert!(
            (buf[2] - b).abs() < 1e-6,
            "B drifted at threshold: {}",
            buf[2]
        );
    }

    #[test]
    fn ceiling_hits_pure_luma() {
        // At `max == ceiling`, smoothstep evaluates to 1. The output is
        // the pixel's luminance on every channel.
        let (threshold, ceiling) = (0.95_f32, 1.20);
        let (r, g, b) = (1.20_f32, 0.4, 0.2);
        let y = luma(r, g, b);
        let mut buf = vec![r, g, b];
        apply_highlight_recovery(&mut buf, threshold, ceiling);
        for (i, v) in buf.iter().enumerate() {
            assert!(
                (v - y).abs() < 1e-5,
                "channel {i} should equal luma {y}, got {v}"
            );
        }
    }

    #[test]
    fn above_ceiling_stays_neutral_at_luma() {
        // Above ceiling the mix reaches (Y, Y, Y) and stops there.
        let (threshold, ceiling) = (0.95_f32, 1.20);
        let (r, g, b) = (1.6_f32, 0.4, 0.2);
        let y = luma(r, g, b);
        let mut buf = vec![r, g, b];
        apply_highlight_recovery(&mut buf, threshold, ceiling);
        for (i, v) in buf.iter().enumerate() {
            assert!(
                (v - y).abs() < 1e-5,
                "channel {i} above ceiling should equal luma {y}, got {v}"
            );
        }
    }

    #[test]
    fn monotonic_progression_between_threshold_and_ceiling() {
        // The recovery blends the input toward (Y, Y, Y) by some fraction
        // `t`, which should rise monotonically from 0 to 1 as `max` sweeps
        // from threshold to ceiling. We reverse-engineer `t` from the
        // output using the mix identity
        //   c_out = c + (Y − c) · t   ⇒   t = (c_out − c) / (Y − c)
        // on the max channel (which has the largest denominator and so
        // the least numerical noise).
        //
        // Recovering `t` this way pins down the right invariant
        // irrespective of how `max`, luma, and individual channel values
        // shift together across the transition.
        let (threshold, ceiling) = (0.95_f32, 1.20);
        let (g, b) = (0.3_f32, 0.1);
        let mut previous_t = -1.0_f32;
        for i in 0..=64 {
            let r = threshold + (ceiling - threshold) * (i as f32 / 64.0);
            let y_in = luma(r, g, b);
            let mut buf = vec![r, g, b];
            apply_highlight_recovery(&mut buf, threshold, ceiling);
            let denom = y_in - r;
            assert!(
                denom.abs() > 1e-4,
                "r={r}: Y too close to r for a clean t read"
            );
            let t = (buf[0] - r) / denom;
            assert!(
                t >= previous_t - 1e-5 && (0.0..=1.0 + 1e-5).contains(&t),
                "t non-monotonic or out of [0, 1] at r={r}: previous {previous_t}, current {t}"
            );
            previous_t = t;
        }
    }

    #[test]
    fn hue_direction_does_not_invert() {
        // Before recovery, R > G > B (a saturated warm pixel). After
        // partial recovery, the three channels should move toward Y but
        // the ordering should not flip: R must stay >= G, G must stay
        // >= B. Otherwise we'd have introduced a hue inversion.
        let (r_in, g_in, b_in) = (1.2_f32, 0.6, 0.3);
        let mut buf = vec![r_in, g_in, b_in];
        apply_default_highlight_recovery(&mut buf);
        assert!(
            buf[0] >= buf[1] - 1e-5,
            "R fell below G: {} vs {}",
            buf[0],
            buf[1]
        );
        assert!(
            buf[1] >= buf[2] - 1e-5,
            "G fell below B: {} vs {}",
            buf[1],
            buf[2]
        );
        // And the deltas should be non-negative (no channel overshoots
        // luma and comes out the other side).
        let y = luma(r_in, g_in, b_in);
        assert!(buf[0] >= y - 1e-5, "R under-shot luma: {} vs {y}", buf[0]);
        assert!(buf[1] <= buf[0] + 1e-5);
        assert!(buf[2] <= y + 1e-5, "B over-shot luma: {} vs {y}", buf[2]);
    }

    #[test]
    fn negative_inputs_pass_through() {
        // Negative values shouldn't occur post-exposure in practice (the
        // camera matrix can produce them on wildly out-of-gamut inputs,
        // but they're usually tiny). Either way, max of a pixel with
        // negative values is still below the threshold, so recovery is a
        // no-op.
        let mut buf = vec![-0.1_f32, -0.05, 0.2];
        let expected = buf.clone();
        apply_default_highlight_recovery(&mut buf);
        for (got, want) in buf.iter().zip(expected.iter()) {
            assert!(
                (got - want).abs() < 1e-6,
                "negative input drifted: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn nan_input_passes_through() {
        // NaN pixels are left alone. The tone curve downstream folds them
        // to zero already; we don't need a second guard here.
        let mut buf = vec![f32::NAN, 0.5, 0.5];
        apply_default_highlight_recovery(&mut buf);
        assert!(buf[0].is_nan(), "NaN got mangled: {}", buf[0]);
        assert!((buf[1] - 0.5).abs() < 1e-6);
        assert!((buf[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn degenerate_params_fall_back_to_hard_step() {
        // threshold == ceiling: the smoothstep denominator is zero. We
        // fall back to a hard step — any `max > threshold` goes straight
        // to (Y, Y, Y), anything at or below is untouched.
        let (threshold, ceiling) = (0.9_f32, 0.9);

        // max == threshold: no-op (smoothstep returns 0 when x <= a).
        let mut buf = vec![0.9_f32, 0.3, 0.1];
        apply_highlight_recovery(&mut buf, threshold, ceiling);
        assert!((buf[0] - 0.9).abs() < 1e-6);
        assert!((buf[1] - 0.3).abs() < 1e-6);
        assert!((buf[2] - 0.1).abs() < 1e-6);

        // max > threshold: hard desaturate to Y.
        let (r, g, b) = (1.1_f32, 0.3, 0.1);
        let y = luma(r, g, b);
        let mut buf = vec![r, g, b];
        apply_highlight_recovery(&mut buf, threshold, ceiling);
        for v in &buf {
            assert!(
                (v - y).abs() < 1e-5,
                "hard-step output should equal luma {y}, got {v}"
            );
        }
    }

    #[test]
    fn threshold_greater_than_ceiling_is_hard_step() {
        // Inverted parameters: we still want a sensible fallback rather
        // than panicking or dividing by zero. Matches the
        // `degenerate_params_fall_back_to_hard_step` behaviour above.
        let (threshold, ceiling) = (1.0_f32, 0.5);
        let (r, g, b) = (1.5_f32, 0.3, 0.1);
        let y = luma(r, g, b);
        let mut buf = vec![r, g, b];
        apply_highlight_recovery(&mut buf, threshold, ceiling);
        for v in &buf {
            assert!(
                (v - y).abs() < 1e-5,
                "inverted-params: expected {y}, got {v}"
            );
        }
    }
}
