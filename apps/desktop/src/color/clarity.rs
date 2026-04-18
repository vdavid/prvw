//! Local contrast ("clarity") enhancement for RAW output.
//!
//! Same separable-Gaussian unsharp-mask algorithm [`super::sharpen`] uses,
//! but with a much larger radius. Capture sharpening (`σ ≈ 0.8` px) works at
//! the pixel edge level, so its effect is only visible at 100 % zoom; at
//! fit-to-window zoom the display downsample averages it out. Clarity
//! (`σ ≈ 10` px) works on midtone features — shape silhouettes, textures,
//! the mid-frequency content — which survives display downscaling. That's
//! why Lightroom's "Clarity" and Affinity's "Detail Refinement" slider
//! make photos look noticeably crisper at ALL zoom levels.
//!
//! Luminance-only, edge-replication, rayon row-parallel — all the same
//! invariants as capture sharpening (see `sharpen.rs` for the full rationale
//! on luminance-only vs. per-channel, Rec.709 weights, post-ICC slot, etc.).
//! This module is a thin wrapper: the defaults and the public names differ,
//! the underlying math is shared. If one day we want clarity's math to
//! diverge (different kernel, different space), we extract the core out of
//! `sharpen.rs` and both call it — but for now, the semantic split is in
//! the call sites and defaults, not the algorithm.
//!
//! ## Defaults
//!
//! - **Radius σ = 10 px.** Midtone features without smearing shape
//!   outlines. Affinity's "Detail Refinement" at its default "25 %" appears
//!   to sit around σ = 20-25 px; we stay slightly more conservative so
//!   halos don't appear on high-contrast edges.
//! - **Amount = 0.40.** Moderate — Affinity's default reads around 0.5-0.6
//!   from visual inspection, but 0.4 gives pleasant lift without pushing
//!   into the "processed" look some users dislike.
//!
//! Users tune both via the Settings → RAW → Detail sliders (radius in
//! pixels, amount as a 0.0-1.0 float).
//!
//! ## Pipeline position
//!
//! Runs in display-space RGBA8 / RGBA16F **before** capture sharpening, so
//! the ordering is clarity (mid-frequency lift) → capture sharpening (fine
//! edges). Both operate on luminance only; their effects compose cleanly.
//!
//! ## Perf
//!
//! At σ = 10 the kernel is 61 taps. A 20 MP RGBA8 buffer: 2 separable
//! passes × 61 × 20 M ≈ 2.4 B FMAs. On Apple Silicon with NEON and rayon
//! that's typically 200-400 ms. Not free, but cheap enough for "always
//! on" default behavior. Disable via the toggle if it's a bottleneck on a
//! slow machine.

use super::sharpen;

/// Default radius for the local-contrast Gaussian, in pixels. Matches the
/// Settings → RAW → Detail slider's default position.
pub const DEFAULT_RADIUS: f32 = 10.0;
/// Default unsharp-mask amount for the local-contrast pass. 0.0 = no
/// effect, 1.0 = aggressive.
pub const DEFAULT_AMOUNT: f32 = 0.40;

/// Apply a local-contrast pass to an RGBA8 buffer. `radius` is the
/// Gaussian σ in pixels; `amount` scales the unsharp-mask contribution.
/// Production callers pass [`DEFAULT_RADIUS`] / [`DEFAULT_AMOUNT`] unless
/// the user has moved the Settings → RAW → Detail sliders.
pub fn apply_clarity_rgba8_inplace_with(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    radius: f32,
    amount: f32,
) {
    sharpen::sharpen_rgba8_inplace_with(rgba, width, height, radius, amount);
}

/// HDR path equivalent of [`apply_clarity_rgba8_inplace_with`]: runs the
/// same luminance-only unsharp mask on a half-float RGBA16F buffer,
/// preserving above-white highlights.
pub fn apply_clarity_rgba16f_inplace_with(
    rgba: &mut [u16],
    width: u32,
    height: u32,
    radius: f32,
    amount: f32,
) {
    sharpen::sharpen_rgba16f_inplace_with(rgba, width, height, radius, amount);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat buffer stays flat under clarity — no edges = no mid-frequency
    /// content for the unsharp mask to amplify.
    #[test]
    fn flat_rgba8_unchanged() {
        let mut rgba: Vec<u8> = (0..16 * 16).flat_map(|_| [128u8, 128, 128, 255]).collect();
        let expected = rgba.clone();
        apply_clarity_rgba8_inplace_with(&mut rgba, 16, 16, DEFAULT_RADIUS, DEFAULT_AMOUNT);
        assert_eq!(rgba, expected);
    }

    /// Amount 0.0 is a no-op regardless of radius.
    #[test]
    fn amount_zero_is_noop() {
        // A striped pattern so there's midtone content to lift.
        let mut rgba = Vec::with_capacity(16 * 16 * 4);
        for y in 0..16 {
            for _ in 0..16 {
                let v = if y % 2 == 0 { 64 } else { 192 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let expected = rgba.clone();
        apply_clarity_rgba8_inplace_with(&mut rgba, 16, 16, 10.0, 0.0);
        assert_eq!(rgba, expected);
    }

    /// Alpha is never touched.
    #[test]
    fn alpha_preserved() {
        let mut rgba = Vec::with_capacity(8 * 8 * 4);
        for i in 0..64 {
            let v = (i * 4).min(255) as u8;
            rgba.extend_from_slice(&[v, v, v, (i as u8).wrapping_mul(7)]);
        }
        let alphas_before: Vec<u8> = rgba.iter().skip(3).step_by(4).copied().collect();
        apply_clarity_rgba8_inplace_with(&mut rgba, 8, 8, DEFAULT_RADIUS, DEFAULT_AMOUNT);
        let alphas_after: Vec<u8> = rgba.iter().skip(3).step_by(4).copied().collect();
        assert_eq!(alphas_before, alphas_after);
    }
}
