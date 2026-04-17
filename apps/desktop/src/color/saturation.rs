//! Global saturation boost for RAW rendering.
//!
//! Applied after the tone curve and before the ICC transform, still in
//! linear Rec.2020 space. Pushes chroma out from the luminance axis by a
//! small multiplicative factor, restoring the "vibrancy" Apple/Affinity
//! get from their camera-specific tuning tables without our needing to
//! ship per-camera profiles yet.
//!
//! ## Why a saturation boost
//!
//! Even with the luminance-only tone curve preserving chroma through the
//! highlight shoulder, the overall output still reads a touch flatter than
//! Preview.app and Affinity. Those apps apply per-camera "look" tables
//! (Apple ships proprietary ones, Adobe's DCP profiles do similar) that
//! bend saturation upward at most brightness levels. We can approximate
//! the net effect with a single global chroma scale until we land per-camera
//! profiles in Phase 3.
//!
//! ## Where in the pipeline
//!
//! Post-tone-curve, pre-ICC, in **linear Rec.2020**. Linear space is where
//! chroma math is most perceptually correct: a +8 % scale on `(R - Y)` in
//! linear light is a consistent perceptual boost across bright and dark
//! pixels. Doing it post-ICC on gamma-encoded RGB8 would land uneven lifts
//! — saturated shadows would push harder than saturated highlights because
//! of the gamma curve.
//!
//! ## Algorithm
//!
//! The classic "scale chroma around luma" formula:
//!
//! ```text
//! Y      = luma(R, G, B)                   // Rec.2020 weights
//! R_out  = Y + (R - Y) * (1 + boost)
//! G_out  = Y + (G - Y) * (1 + boost)
//! B_out  = Y + (B - Y) * (1 + boost)
//! ```
//!
//! Preserves hue exactly. Preserves luminance exactly (every channel's
//! delta-from-luma is symmetric around Y, so the weighted sum stays the
//! same). Only the distance from the luminance axis scales.
//!
//! ## Value range
//!
//! The formula can produce values outside `[0, 1]` — amplifying chroma on
//! near-saturated pixels can push one channel above 1 or below 0. We don't
//! clamp here; the downstream ICC transform and `rec2020_to_rgba8` already
//! clamp for us, and keeping the wide-gamut f32 signal alive through the
//! ICC transform is part of why we're in linear Rec.2020 in the first
//! place.
//!
//! ## Safety invariants (enforced by unit tests)
//!
//! - Neutral gray `(v, v, v)` is unchanged regardless of boost (chroma = 0,
//!   so the delta is 0).
//! - Pure primary `(1, 0, 0)` retains its hue (R stays the max; G and B
//!   stay equal).
//! - A saturated color's chroma grows by the expected factor `(1 + boost)`.
//! - Boost of 0 is a no-op.

use rayon::prelude::*;

use super::tone_curve::{REC2020_LUMA_B, REC2020_LUMA_G, REC2020_LUMA_R};

/// Default global saturation boost, applied after the tone curve and before
/// the ICC transform. +8 % (`0.08`) is the smallest value that noticeably
/// closes the "vibrancy" gap against Preview.app and Affinity on real photo
/// content without tipping into "over-processed" territory; see
/// `docs/notes/raw-support-phase2.md` for the Phase 2.5a decision.
pub const DEFAULT_SATURATION_BOOST: f32 = 0.08;

/// Apply a global saturation boost to a flat RGB f32 buffer in place. Layout
/// is `[R0, G0, B0, R1, G1, B1, …]`; length must be a multiple of 3.
///
/// `boost == 0.0` is a cheap no-op. Negative `boost` desaturates (down to
/// `-1.0` which would collapse every pixel to neutral luminance); we don't
/// clamp the argument here — callers are expected to pass a sane default
/// like [`DEFAULT_SATURATION_BOOST`].
pub fn apply_saturation_boost(rgb: &mut [f32], boost: f32) {
    if boost == 0.0 {
        return;
    }
    let scale = 1.0 + boost;
    rgb.par_chunks_exact_mut(3).for_each(|pixel| {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let y = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;
        pixel[0] = y + (r - y) * scale;
        pixel[1] = y + (g - y) * scale;
        pixel[2] = y + (b - y) * scale;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_boost_is_noop() {
        let mut buf = vec![0.1_f32, 0.4, 0.9, 0.5, 0.5, 0.5];
        let expected = buf.clone();
        apply_saturation_boost(&mut buf, 0.0);
        assert_eq!(buf, expected);
    }

    #[test]
    fn empty_buffer_does_not_panic() {
        let mut buf: Vec<f32> = Vec::new();
        apply_saturation_boost(&mut buf, DEFAULT_SATURATION_BOOST);
        assert!(buf.is_empty());
    }

    #[test]
    fn neutral_gray_is_unchanged() {
        // Every channel equals luma, so (R - Y) = 0 for every channel. Any
        // boost multiplies 0 by (1 + boost) and lands back on Y.
        for gray in [0.0_f32, 0.1, 0.25, 0.5, 0.75, 1.0] {
            let mut buf = vec![gray, gray, gray];
            apply_saturation_boost(&mut buf, DEFAULT_SATURATION_BOOST);
            for (i, v) in buf.iter().enumerate() {
                assert!(
                    (v - gray).abs() < 1e-5,
                    "gray {gray} channel {i} drifted to {v}"
                );
            }
        }
    }

    #[test]
    fn pure_primary_retains_hue() {
        // Pure red (1, 0, 0): after the boost, R should still be the max,
        // G and B should remain equal to each other (they both get scaled
        // identically from their (−Y) starting point).
        let mut buf = vec![1.0_f32, 0.0, 0.0];
        apply_saturation_boost(&mut buf, DEFAULT_SATURATION_BOOST);
        assert!(buf[0] > buf[1], "red no longer dominant");
        assert!(buf[0] > buf[2], "red no longer dominant");
        assert!(
            (buf[1] - buf[2]).abs() < 1e-6,
            "G and B diverged: {} vs {}",
            buf[1],
            buf[2]
        );
    }

    #[test]
    fn saturation_grows_by_boost_factor() {
        // For any non-gray pixel, the distance from the luminance axis
        // `sqrt((R-Y)^2 + (G-Y)^2 + (B-Y)^2)` should grow by exactly
        // (1 + boost) after the boost. Numerical check on a mid-saturation
        // pixel.
        let boost = DEFAULT_SATURATION_BOOST;
        let r = 0.6_f32;
        let g = 0.3_f32;
        let b = 0.1_f32;
        let y = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;
        let chroma_in = ((r - y).powi(2) + (g - y).powi(2) + (b - y).powi(2)).sqrt();

        let mut buf = vec![r, g, b];
        apply_saturation_boost(&mut buf, boost);
        let (r_out, g_out, b_out) = (buf[0], buf[1], buf[2]);
        let y_out = REC2020_LUMA_R * r_out + REC2020_LUMA_G * g_out + REC2020_LUMA_B * b_out;
        let chroma_out =
            ((r_out - y_out).powi(2) + (g_out - y_out).powi(2) + (b_out - y_out).powi(2)).sqrt();

        let want = chroma_in * (1.0 + boost);
        assert!(
            (chroma_out - want).abs() < 1e-5,
            "chroma in={chroma_in}, out={chroma_out}, want={want}"
        );
    }

    #[test]
    fn luminance_is_preserved() {
        // The formula is symmetric around Y: luminance of the output equals
        // luminance of the input to within float rounding.
        let r = 0.6_f32;
        let g = 0.3_f32;
        let b = 0.1_f32;
        let y_in = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;

        let mut buf = vec![r, g, b];
        apply_saturation_boost(&mut buf, DEFAULT_SATURATION_BOOST);
        let y_out = REC2020_LUMA_R * buf[0] + REC2020_LUMA_G * buf[1] + REC2020_LUMA_B * buf[2];
        assert!(
            (y_out - y_in).abs() < 1e-5,
            "luminance drifted: in={y_in}, out={y_out}"
        );
    }

    #[test]
    fn negative_boost_desaturates() {
        // Negative boost should shrink chroma. Passing -1.0 collapses every
        // pixel to its luminance value (pure gray).
        let r = 0.8_f32;
        let g = 0.2_f32;
        let b = 0.1_f32;
        let y = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;

        let mut buf = vec![r, g, b];
        apply_saturation_boost(&mut buf, -1.0);
        for (i, v) in buf.iter().enumerate() {
            assert!(
                (v - y).abs() < 1e-5,
                "desaturate-to-gray channel {i}: {v} != {y}"
            );
        }
    }
}
