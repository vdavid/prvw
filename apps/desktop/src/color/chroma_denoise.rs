//! Chroma noise reduction: small separable Gaussian blur on Cb / Cr,
//! leaving luminance sharp. Cleans the dominant noise source in RAW files
//! (chroma noise is typically 3-5x worse than luma noise per the sensor's
//! Bayer pattern) at the cost of slightly softened color edges, a cheap
//! quality win that matches the default behavior of Preview and Affinity.
//!
//! ## Algorithm
//!
//! For each pixel:
//!
//! 1. Convert linear Rec.2020 RGB to Y (luminance) + Cb + Cr using the
//!    Rec.2020 weights `Y = 0.2627 R + 0.6780 G + 0.0593 B`,
//!    `Cb = B - Y`, `Cr = R - Y`. Luminance is preserved exactly; Cb / Cr
//!    carry every hue/chroma variation the pixel had.
//! 2. Run a separable Gaussian blur on the Cb plane, then on the Cr plane.
//!    Luminance is not touched, so edge detail and micro-contrast pass
//!    through unchanged.
//! 3. Reconstruct RGB: `R = Y + Cr`, `B = Y + Cb`,
//!    `G = (Y - 0.2627 R - 0.0593 B) / 0.6780`. Algebra falls straight out
//!    of the forward formulas.
//!
//! ## Pipeline slot
//!
//! Runs in linear Rec.2020, after `camera_to_linear_rec2020` (and the
//! lens-correction geometry pass) but before `baseline_exposure`. Chroma
//! noise is rawest closest to demosaic output, so cleaning it there is the
//! cheapest and least destructive. Later exposure and tone-curve passes
//! can't re-introduce chroma noise that isn't there — they scale luminance
//! only.
//!
//! ## Parameters
//!
//! - **Sigma**: Gaussian blur radius, in pixels. Small `σ` (~1.0) cleans
//!   noise without losing color edges; larger `σ` (~3.0) removes more noise
//!   but smears colored details into neighbors.
//!   Default `σ = 1.5 px` — mild, matching the consumer viewer defaults
//!   (Preview, Affinity).
//! - **Strength**: global mix factor between the original and fully blurred
//!   Cb / Cr. `0.0` = pass-through; `1.0` = full blur. Defaults to `1.0`;
//!   the user-facing toggle gates the whole stage on/off instead of
//!   exposing a continuous slider in v1.
//!
//! Kernel width is `2 * ceil(3σ) + 1` (same rule as `color::sharpen`). For
//! `σ = 1.5` that's 11 taps — cheap to evaluate, especially since we run
//! it on Cb / Cr only (two passes, not three).
//!
//! ## Performance
//!
//! Rayon-parallel over rows, same pattern as `color::sharpen`. `f32`
//! throughout. Inner blur rows are annotated with `#[multiversion]`
//! (NEON / AVX2+FMA) so the compiler emits optimised variants — reuses the
//! infrastructure Phase 6.3 brought into `lens_correction.rs`. Expected
//! cost is ~15-25 ms on a 20 MP image.
//!
//! ## Edge handling
//!
//! Edge replication (clamp-to-edge). The kernel samples beyond the buffer
//! boundary reuse the outermost pixel. Matches `color::sharpen`.
//!
//! ## Safety invariants (enforced by unit tests)
//!
//! - Flat-color buffers pass through unchanged (Cb, Cr are constant,
//!   blurring returns the same plane, reconstruction yields the input).
//! - `strength = 0` is a bit-exact no-op.
//! - A pure luminance pattern (neutral grays only) comes out unchanged.
//! - Luma is preserved exactly per-pixel (within f32 round-trip error)
//!   even when Cb / Cr blur.

use multiversion::multiversion;
use rayon::prelude::*;

/// Default Gaussian blur radius for chroma denoise, in pixels. `1.5` is
/// mild — cleans the low-frequency chroma noise that shows up in shadows
/// and flat-color areas without visibly smearing saturated edges. Matches
/// the behavior Preview.app and Affinity apply by default.
pub const DEFAULT_SIGMA: f32 = 1.5;

/// Default mix strength for chroma denoise. `1.0` = fully-blurred chroma
/// replaces the input. v1 exposes the module as an on/off toggle, so this
/// never changes in production; kept as a constant so future slider work
/// can reach it without reshaping the function signatures.
pub const DEFAULT_STRENGTH: f32 = 1.0;

/// Rec.2020 luminance weight for red.
const LUMA_R: f32 = 0.2627;
/// Rec.2020 luminance weight for green.
const LUMA_G: f32 = 0.6780;
/// Rec.2020 luminance weight for blue.
const LUMA_B: f32 = 0.0593;

/// Apply chroma denoise in place with the default [`DEFAULT_SIGMA`] and
/// [`DEFAULT_STRENGTH`]. `rgb` must be a flat linear-Rec.2020 buffer laid
/// out as `[r, g, b, r, g, b, ...]`, length `width * height * 3`. Mismatch
/// is a silent no-op, matching the rest of the color pipeline's defensive
/// posture.
pub fn apply_default_chroma_denoise(rgb: &mut [f32], width: u32, height: u32) {
    apply_chroma_denoise(rgb, width, height, DEFAULT_SIGMA, DEFAULT_STRENGTH);
}

/// Parametric variant of [`apply_default_chroma_denoise`]. Blurs the
/// Cb / Cr planes with a Gaussian of radius `sigma`, then mixes the result
/// back in with weight `strength` (`0.0` = no blur, `1.0` = full blur).
pub fn apply_chroma_denoise(rgb: &mut [f32], width: u32, height: u32, sigma: f32, strength: f32) {
    if width == 0 || height == 0 {
        return;
    }
    let pixels = (width as usize) * (height as usize);
    if rgb.len() != pixels * 3 {
        return;
    }
    if pixels < 2 {
        return;
    }
    if strength <= 0.0 || sigma <= 0.0 {
        return;
    }

    let kernel = gaussian_kernel_1d(sigma);
    let radius = kernel.len() / 2;

    // Derive Cb and Cr planes from RGB. Keep a parallel Y plane so we can
    // reconstruct without a second pass over the RGB buffer.
    let mut luma = vec![0.0_f32; pixels];
    let mut cb = vec![0.0_f32; pixels];
    let mut cr = vec![0.0_f32; pixels];
    luma.par_iter_mut()
        .zip(cb.par_iter_mut())
        .zip(cr.par_iter_mut())
        .zip(rgb.par_chunks_exact(3))
        .for_each(|(((y_slot, cb_slot), cr_slot), px)| {
            let r = px[0];
            let g = px[1];
            let b = px[2];
            let y = LUMA_R * r + LUMA_G * g + LUMA_B * b;
            *y_slot = y;
            *cb_slot = b - y;
            *cr_slot = r - y;
        });

    // Separable blur, scratch-buffered. Each plane needs its own scratch;
    // reusing between Cb and Cr is cheap (same allocation, overwritten).
    let mut scratch = vec![0.0_f32; pixels];
    let mut cb_blurred = vec![0.0_f32; pixels];
    let mut cr_blurred = vec![0.0_f32; pixels];
    blur_horizontal(&cb, &mut scratch, width, height, &kernel, radius);
    blur_vertical(&scratch, &mut cb_blurred, width, height, &kernel, radius);
    blur_horizontal(&cr, &mut scratch, width, height, &kernel, radius);
    blur_vertical(&scratch, &mut cr_blurred, width, height, &kernel, radius);

    // Reconstruct RGB in place. When `strength == 1.0` the mix is a
    // straight replacement; otherwise we lerp between the original and the
    // blurred Cb / Cr. The math stays in f32 — the wide-gamut buffer
    // carries f32 values for the rest of the pipeline anyway.
    rgb.par_chunks_exact_mut(3)
        .zip(luma.par_iter())
        .zip(cb.par_iter())
        .zip(cr.par_iter())
        .zip(cb_blurred.par_iter())
        .zip(cr_blurred.par_iter())
        .for_each(|(((((px, &y), &cb_in), &cr_in), &cb_out), &cr_out)| {
            let cb_mix = cb_in + (cb_out - cb_in) * strength;
            let cr_mix = cr_in + (cr_out - cr_in) * strength;
            let r = y + cr_mix;
            let b = y + cb_mix;
            // Invert the Y formula: Y = R*wr + G*wg + B*wb → G = (Y - R*wr - B*wb) / wg
            let g = (y - LUMA_R * r - LUMA_B * b) / LUMA_G;
            px[0] = r;
            px[1] = g;
            px[2] = b;
        });
}

/// Build a normalised 1D Gaussian kernel covering ±3σ. Matches
/// `color::sharpen::gaussian_kernel_1d` in shape and numerics — kept as a
/// local copy so changes to sharpening's kernel math (should they ever
/// happen) don't accidentally affect chroma denoise.
fn gaussian_kernel_1d(sigma: f32) -> Vec<f32> {
    let sigma = sigma.max(1e-3);
    let radius = (3.0 * sigma).ceil() as usize;
    let len = 2 * radius + 1;
    let mut kernel = Vec::with_capacity(len);
    let two_sigma_sq = 2.0 * sigma * sigma;
    let mut sum = 0.0_f32;
    for i in 0..len {
        let x = i as f32 - radius as f32;
        let w = (-(x * x) / two_sigma_sq).exp();
        kernel.push(w);
        sum += w;
    }
    for w in kernel.iter_mut() {
        *w /= sum;
    }
    kernel
}

/// Horizontal 1D Gaussian blur with edge replication. Rows run in parallel.
fn blur_horizontal(
    input: &[f32],
    output: &mut [f32],
    width: u32,
    height: u32,
    kernel: &[f32],
    radius: usize,
) {
    let w = width as usize;
    let _ = height;
    output
        .par_chunks_exact_mut(w)
        .zip(input.par_chunks_exact(w))
        .for_each(|(dst_row, src_row)| {
            blur_horizontal_row(dst_row, src_row, kernel, radius, w);
        });
}

/// Vertical 1D Gaussian blur with edge replication. Rows run in parallel;
/// each row samples a column window of the input.
fn blur_vertical(
    input: &[f32],
    output: &mut [f32],
    width: u32,
    height: u32,
    kernel: &[f32],
    radius: usize,
) {
    let w = width as usize;
    let h = height as usize;
    output
        .par_chunks_exact_mut(w)
        .enumerate()
        .for_each(|(y, dst_row)| {
            blur_vertical_row(dst_row, input, kernel, radius, y, w, h);
        });
}

/// Per-row horizontal blur, hot loop. Annotated with `#[multiversion]` so
/// the compiler emits NEON (aarch64) and AVX2+FMA (x86_64) variants
/// alongside the scalar fallback.
#[multiversion(targets("aarch64+neon", "x86_64+avx+avx2+fma"))]
fn blur_horizontal_row(
    dst_row: &mut [f32],
    src_row: &[f32],
    kernel: &[f32],
    radius: usize,
    w: usize,
) {
    for (x, dst) in dst_row.iter_mut().enumerate() {
        let mut acc = 0.0_f32;
        for (k_idx, &k) in kernel.iter().enumerate() {
            let offset = k_idx as isize - radius as isize;
            let sx = clamp_index(x as isize + offset, w);
            acc = k.mul_add(src_row[sx], acc);
        }
        *dst = acc;
    }
}

/// Per-row vertical blur, hot loop. Same `#[multiversion]` treatment as
/// [`blur_horizontal_row`].
#[multiversion(targets("aarch64+neon", "x86_64+avx+avx2+fma"))]
fn blur_vertical_row(
    dst_row: &mut [f32],
    input: &[f32],
    kernel: &[f32],
    radius: usize,
    y: usize,
    w: usize,
    h: usize,
) {
    for (x, dst) in dst_row.iter_mut().enumerate() {
        let mut acc = 0.0_f32;
        for (k_idx, &k) in kernel.iter().enumerate() {
            let offset = k_idx as isize - radius as isize;
            let sy = clamp_index(y as isize + offset, h);
            acc = k.mul_add(input[sy * w + x], acc);
        }
        *dst = acc;
    }
}

/// Clamp a signed index into `[0, len)`. Used for edge replication at the
/// kernel's overhang on borders.
#[inline]
fn clamp_index(i: isize, len: usize) -> usize {
    if i < 0 {
        0
    } else if (i as usize) >= len {
        len - 1
    } else {
        i as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb_filled(width: u32, height: u32, r: f32, g: f32, b: f32) -> Vec<f32> {
        let pixels = (width as usize) * (height as usize);
        let mut buf = Vec::with_capacity(pixels * 3);
        for _ in 0..pixels {
            buf.extend_from_slice(&[r, g, b]);
        }
        buf
    }

    fn luma(r: f32, g: f32, b: f32) -> f32 {
        LUMA_R * r + LUMA_G * g + LUMA_B * b
    }

    #[test]
    fn flat_image_is_unchanged() {
        // Constant-color buffer: Cb / Cr are constant, Gaussian blur of a
        // constant is the same constant, so the reconstruction yields the
        // input (within f32 round-trip error).
        let mut buf = rgb_filled(16, 12, 0.4, 0.55, 0.3);
        let expected = buf.clone();
        apply_default_chroma_denoise(&mut buf, 16, 12);
        for (a, b) in buf.iter().zip(expected.iter()) {
            assert!((a - b).abs() < 1e-5, "flat pixel drifted: {a} vs {b}");
        }
    }

    #[test]
    fn pure_luminance_pattern_is_untouched() {
        // Neutral grays: every pixel has R == G == B, so Cb = Cr = 0
        // everywhere. Blurring a zero plane yields zero, and
        // reconstruction gives back the original grays. A black-to-white
        // checkerboard is the stress test here — luma micro-contrast
        // must not be softened.
        let width = 8_u32;
        let height = 8_u32;
        let pixels = (width * height) as usize;
        let mut buf = Vec::with_capacity(pixels * 3);
        for y in 0..height {
            for x in 0..width {
                let v = if (x + y) % 2 == 0 { 0.0 } else { 1.0 };
                buf.extend_from_slice(&[v, v, v]);
            }
        }
        let expected = buf.clone();
        apply_default_chroma_denoise(&mut buf, width, height);
        for (i, (a, b)) in buf.iter().zip(expected.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-4,
                "checkerboard luma {i} drifted: {a} vs {b}"
            );
        }
    }

    #[test]
    fn zero_strength_is_identity() {
        // `strength = 0` short-circuits. Confirm the buffer is untouched
        // even on a buffer with real chroma variation.
        let width = 8_u32;
        let height = 8_u32;
        let pixels = (width * height) as usize;
        let mut buf = Vec::with_capacity(pixels * 3);
        for y in 0..height {
            for x in 0..width {
                let r = x as f32 / width as f32;
                let g = y as f32 / height as f32;
                let b = ((x + y) % 3) as f32 / 3.0;
                buf.extend_from_slice(&[r, g, b]);
            }
        }
        let expected = buf.clone();
        apply_chroma_denoise(&mut buf, width, height, DEFAULT_SIGMA, 0.0);
        assert_eq!(buf, expected);
    }

    #[test]
    fn zero_sigma_is_identity() {
        // `sigma = 0` short-circuits the same way.
        let mut buf = rgb_filled(8, 8, 0.2, 0.7, 0.4);
        let expected = buf.clone();
        apply_chroma_denoise(&mut buf, 8, 8, 0.0, DEFAULT_STRENGTH);
        assert_eq!(buf, expected);
    }

    #[test]
    fn luminance_is_preserved_pointwise() {
        // For every pixel, the Y computed from the output RGB must match
        // the Y computed from the input RGB (within f32 arithmetic
        // tolerance). This is the single most important invariant: the
        // whole point of chroma denoise is "don't touch luma".
        let width = 16_u32;
        let height = 16_u32;
        let pixels = (width * height) as usize;
        let mut buf = Vec::with_capacity(pixels * 3);
        let mut luma_before = Vec::with_capacity(pixels);
        for y in 0..height {
            for x in 0..width {
                let r = (x as f32 * 31.0).sin() * 0.3 + 0.5;
                let g = (y as f32 * 17.0).cos() * 0.3 + 0.5;
                let b = ((x + y) as f32 * 11.0).sin() * 0.3 + 0.5;
                buf.extend_from_slice(&[r, g, b]);
                luma_before.push(luma(r, g, b));
            }
        }
        apply_default_chroma_denoise(&mut buf, width, height);
        for (i, chunk) in buf.chunks_exact(3).enumerate() {
            let y_after = luma(chunk[0], chunk[1], chunk[2]);
            assert!(
                (y_after - luma_before[i]).abs() < 1e-4,
                "luma drifted at pixel {i}: before {}, after {y_after}",
                luma_before[i]
            );
        }
    }

    #[test]
    fn colored_impulse_blurs_into_neighbors() {
        // A single saturated pixel on a neutral field. After denoise its
        // chroma should spread into the neighbors — their R / G / B
        // should drift away from the neutral value.
        let width = 11_u32;
        let height = 11_u32;
        let mut buf = rgb_filled(width, height, 0.5, 0.5, 0.5);
        let center = ((height / 2) * width + (width / 2)) as usize;
        buf[center * 3] = 1.0;
        buf[center * 3 + 1] = 0.0;
        buf[center * 3 + 2] = 0.0;

        let before = buf.clone();
        apply_default_chroma_denoise(&mut buf, width, height);

        // The immediate neighbour (center + 1) started neutral. After
        // blurring the red pixel's chroma into it, its Cb / Cr should be
        // nonzero — which means R or B diverged from 0.5.
        let right = center + 1;
        let r_after = buf[right * 3];
        let g_after = buf[right * 3 + 1];
        let b_after = buf[right * 3 + 2];
        let drift = (r_after - before[right * 3]).abs()
            + (g_after - before[right * 3 + 1]).abs()
            + (b_after - before[right * 3 + 2]).abs();
        assert!(
            drift > 0.01,
            "neighbour pixel didn't pick up chroma: R={r_after} G={g_after} B={b_after}"
        );
    }

    #[test]
    fn mismatched_buffer_length_is_noop() {
        let mut buf = vec![0.2_f32; 5];
        let before = buf.clone();
        apply_default_chroma_denoise(&mut buf, 8, 8);
        assert_eq!(buf, before);
    }

    #[test]
    fn zero_dimensions_is_noop() {
        let mut buf: Vec<f32> = Vec::new();
        apply_default_chroma_denoise(&mut buf, 0, 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn single_pixel_is_noop() {
        let mut buf = vec![0.2_f32, 0.5, 0.7];
        let before = buf.clone();
        apply_default_chroma_denoise(&mut buf, 1, 1);
        assert_eq!(buf, before);
    }

    #[test]
    fn gaussian_kernel_sums_to_one() {
        for sigma in [0.8_f32, 1.5, 2.0, 3.0] {
            let k = gaussian_kernel_1d(sigma);
            let sum: f32 = k.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "sigma {sigma} sum {sum}");
        }
    }

    /// Rough perf sanity check. `#[ignore]`'d so it doesn't run in CI.
    /// `cargo test --release color::chroma_denoise::tests::chroma_denoise_20mp_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn chroma_denoise_20mp_bench() {
        let width: u32 = 5472;
        let height: u32 = 3648;
        let mut buf = rgb_filled(width, height, 0.4, 0.55, 0.3);
        // Warm up once.
        apply_default_chroma_denoise(&mut buf, width, height);
        let mut times = vec![];
        for _ in 0..5 {
            let t = std::time::Instant::now();
            apply_default_chroma_denoise(&mut buf, width, height);
            times.push(t.elapsed().as_millis());
        }
        println!("Chroma denoise 20 MP times (ms): {times:?}");
    }
}
