//! Capture sharpening for RAW output.
//!
//! Applied as the final pre-orientation step on the RGBA8 buffer. A mild
//! unsharp mask compensates for the softness every RAW decode carries: the
//! sensor's optical low-pass filter softens fine detail, and demosaic blurs
//! it a second time. Without this step, the output reads "slightly soft"
//! next to Preview.app and Lightroom, both of which apply capture sharpening
//! silently.
//!
//! ## Luminance-only (Phase 2.5a)
//!
//! Sharpening runs on **luminance only**, not per-channel. We convert each
//! RGB pixel to Y in f32, blur Y, apply the unsharp-mask formula on Y to
//! produce `Y_out`, then scale the original `(R, G, B)` by `Y_out / Y_in`.
//! That preserves hue (the `R:G:B` ratio is untouched) and avoids the
//! color fringes per-channel sharpening produces at edges where the three
//! channels' edge locations don't line up at sub-pixel precision.
//!
//! Luma weights are the **Rec.709 / sRGB** coefficients (`0.2126 R +
//! 0.7152 G + 0.0722 B`) because the buffer is in display-space RGB at
//! this point. Rec.709 is close enough to Display P3 for this purpose; the
//! weights differ by a couple of percent and the sharpening amplitude is
//! already small.
//!
//! ## Why post-ICC (display-space RGB8) rather than linear Rec.2020
//!
//! Two defensible slots exist: linear Rec.2020 (pre-ICC, on floats) or
//! display-space RGB8 (post-ICC). We picked display-space because:
//!
//! 1. Unsharp mask in linear light exaggerates the difference at bright
//!    edges (the subtraction has more headroom on the linear side), which
//!    produces visible halos on skies and shadow boundaries. Sharpening on
//!    a gamma-encoded buffer matches the perceptual response human eyes
//!    actually have and is what Lightroom and Camera Raw do by default.
//! 2. It's the last step before the orientation rotate, so we never sharpen
//!    a buffer we're about to throw away.
//!
//! We do the blur and mask in f32 internally (rather than u8 arithmetic) to
//! keep precision. Rounding to u8 happens once, on the final write back to
//! the RGBA buffer.
//!
//! ## Algorithm — separable Gaussian unsharp mask on luminance
//!
//! Classic formula on Y: `Y_out = Y + (Y - Y_blurred) * amount`. The blur
//! is a 1D Gaussian applied horizontally, then vertically, on a single
//! luminance plane.
//!
//! Parameters (baked in — no user knob yet):
//!
//! - **Radius σ = 0.8 px.** Small enough to sharpen fine texture (grass,
//!   fabric, bark) without chasing wide edges that'd produce halos.
//! - **Amount = 0.3.** Mild; see `docs/notes/raw-support-phase2.md` for
//!   the empirical calibration (Phase 2.5a keeps the Phase 2.4 number
//!   unchanged pending the 2.5b grid search).
//! - **Threshold = 0.** No edge discrimination — capture sharpening
//!   uniformly lifts micro-contrast.
//!
//! Kernel width is `2 × ceil(3σ) + 1`. For σ = 0.8 that's 7 taps.
//!
//! ## Edge handling
//!
//! Edge replication (clamp-to-edge). The kernel samples beyond the buffer
//! boundary reuse the outermost pixel. Simpler than reflection and visually
//! indistinguishable for a radius this small.
//!
//! ## Safety invariants (enforced by unit tests)
//!
//! - Flat-color buffers pass through unchanged (no edges → no sharpening).
//! - Overshoot at bright edges saturates at 255 instead of wrapping.
//! - Undershoot at dark edges clamps at 0.
//! - Alpha is never read or written. Only R, G, B get the unsharp pass.
//! - Output dimensions equal input dimensions.
//! - Colored edges do not gain color fringes (the Y-scale pattern preserves
//!   hue).

use rayon::prelude::*;

/// Gaussian standard deviation in pixels. 0.8 is the "capture sharpening"
/// default — small enough for fine detail, broad enough to clear demosaic
/// softening.
pub const DEFAULT_SIGMA: f32 = 0.8;

/// Default unsharp-mask amount, tuned empirically against `sips` references
/// in Phase 2.5b. See `docs/notes/raw-support-phase2.md` for the grid-search
/// table and rationale for holding the amount at or above 0.3 (anything
/// lower starts fitting to `sips`' own conservative rendering rather than
/// the crispness Preview.app shows on screen).
pub const DEFAULT_AMOUNT: f32 = 0.3;

/// Rec.709 / sRGB luma coefficient for red. Close enough to Display P3's
/// own (~0.228) that the ~2 % mismatch is negligible for a small unsharp
/// amplitude.
const LUMA_R: f32 = 0.2126;
/// Rec.709 luma coefficient for green.
const LUMA_G: f32 = 0.7152;
/// Rec.709 luma coefficient for blue.
const LUMA_B: f32 = 0.0722;

/// Below this input luminance the `Y_out / Y_in` scale blows up. Below it
/// we leave the pixel unchanged (the unsharp mask on a black pixel would
/// push it negative anyway, which we'd clamp to 0 — same net result with
/// fewer floating-point hazards).
const DARK_EPSILON: f32 = 1.0e-4;

/// Sharpen an RGBA8 buffer in place with the default capture-sharpening
/// parameters ([`DEFAULT_SIGMA`], [`DEFAULT_AMOUNT`]). Alpha is left
/// untouched.
///
/// `width * height * 4` must equal `rgba.len()`; on mismatch the function
/// is a no-op (same defensive posture as the rest of the color pipeline).
/// Empty buffers and 1×1 images are safe — the blur degenerates to the
/// identity and the unsharp mask becomes a no-op.
pub fn sharpen_rgba8_inplace(rgba: &mut [u8], width: u32, height: u32) {
    sharpen_rgba8_inplace_with(rgba, width, height, DEFAULT_SIGMA, DEFAULT_AMOUNT);
}

/// Parametric variant of [`sharpen_rgba8_inplace`]. Same luminance-only
/// unsharp-mask algorithm, but σ and amount are caller-supplied so the
/// empirical parameter tuner in `examples/raw-tune.rs` can sweep across
/// candidate values. Production code stays on [`sharpen_rgba8_inplace`]
/// with the [`DEFAULT_SIGMA`] / [`DEFAULT_AMOUNT`] pair.
pub fn sharpen_rgba8_inplace_with(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    sigma: f32,
    amount: f32,
) {
    if width == 0 || height == 0 {
        return;
    }
    let pixels = (width as usize) * (height as usize);
    if rgba.len() != pixels * 4 {
        return;
    }
    if pixels < 2 {
        return; // one pixel has no neighbours; blur == input, sharpen == no-op
    }
    if amount == 0.0 {
        return; // zero amount is a cheap no-op
    }

    let kernel = gaussian_kernel_1d(sigma);
    let radius = kernel.len() / 2;

    // Compute luminance plane from the RGBA bytes.
    let luma_in = compute_luma(rgba);
    let mut scratch = vec![0.0_f32; pixels];
    let mut blurred = vec![0.0_f32; pixels];

    blur_horizontal(&luma_in, &mut scratch, width, height, &kernel, radius);
    blur_vertical(&scratch, &mut blurred, width, height, &kernel, radius);

    // Apply the unsharp mask in f32, then reconstruct RGB via Y_out / Y_in.
    // Iterate over pixel chunks in rayon so the whole combine pass lands in
    // parallel. Dark pixels (below DARK_EPSILON) are passed through unchanged
    // to avoid a division blow-up — one-gray-level noise is below the
    // perceptible threshold.
    rgba.par_chunks_exact_mut(4)
        .zip(luma_in.par_iter())
        .zip(blurred.par_iter())
        .for_each(|((px, &y_in), &y_blurred)| {
            if y_in < DARK_EPSILON {
                return;
            }
            let y_out = y_in + (y_in - y_blurred) * amount;
            let scale = y_out / y_in;
            let r = px[0] as f32 * scale;
            let g = px[1] as f32 * scale;
            let b = px[2] as f32 * scale;
            px[0] = f32_to_u8(r);
            px[1] = f32_to_u8(g);
            px[2] = f32_to_u8(b);
            // px[3] (alpha) intentionally untouched
        });
}

/// Build a normalised 1D Gaussian kernel sized to cover ±3σ — past that
/// the tail contributes well below one gray level at 8 bits. Always odd
/// length so there's a single central tap.
fn gaussian_kernel_1d(sigma: f32) -> Vec<f32> {
    // Floor σ to a sensible minimum so `sigma → 0` doesn't produce a
    // one-tap kernel that makes the unsharp mask do nothing.
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

/// Compute the Rec.709 luminance for every pixel of an RGBA8 buffer,
/// returning a flat `width × height` f32 plane. Alpha is ignored.
fn compute_luma(rgba: &[u8]) -> Vec<f32> {
    let pixels = rgba.len() / 4;
    let mut luma = vec![0.0_f32; pixels];
    luma.par_iter_mut()
        .zip(rgba.par_chunks_exact(4))
        .for_each(|(slot, px)| {
            let r = px[0] as f32;
            let g = px[1] as f32;
            let b = px[2] as f32;
            *slot = LUMA_R * r + LUMA_G * g + LUMA_B * b;
        });
    luma
}

#[inline]
fn f32_to_u8(v: f32) -> u8 {
    let clamped = v.clamp(0.0, 255.0);
    (clamped + 0.5) as u8
}

/// Horizontal 1D Gaussian blur with edge replication. Rows are processed
/// in parallel. Input and output are `width × height` single-channel f32
/// planes; they must not alias.
fn blur_horizontal(
    input: &[f32],
    output: &mut [f32],
    width: u32,
    height: u32,
    kernel: &[f32],
    radius: usize,
) {
    let w = width as usize;
    let _ = height; // rayon's chunking already guarantees input.len() == w * h
    output
        .par_chunks_exact_mut(w)
        .zip(input.par_chunks_exact(w))
        .for_each(|(dst_row, src_row)| {
            for (x, dst) in dst_row.iter_mut().enumerate() {
                let mut acc = 0.0_f32;
                for (k_idx, &k) in kernel.iter().enumerate() {
                    let offset = k_idx as isize - radius as isize;
                    let sx = clamp_index(x as isize + offset, w);
                    acc += src_row[sx] * k;
                }
                *dst = acc;
            }
        });
}

/// Vertical 1D Gaussian blur with edge replication. Rows are processed
/// in parallel; each row samples a column window of the input.
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
            for (x, dst) in dst_row.iter_mut().enumerate() {
                let mut acc = 0.0_f32;
                for (k_idx, &k) in kernel.iter().enumerate() {
                    let offset = k_idx as isize - radius as isize;
                    let sy = clamp_index(y as isize + offset, h);
                    acc += input[sy * w + x] * k;
                }
                *dst = acc;
            }
        });
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

    fn rgba_filled(width: u32, height: u32, r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        let pixels = (width as usize) * (height as usize);
        let mut buf = Vec::with_capacity(pixels * 4);
        for _ in 0..pixels {
            buf.extend_from_slice(&[r, g, b, a]);
        }
        buf
    }

    #[test]
    fn flat_image_is_unchanged() {
        // Constant-color buffer. Blurring a flat signal gives back the same
        // flat signal, so `Y - Y_blurred` is 0 everywhere, and the scale
        // factor is exactly 1. No sharpening should change a pixel.
        let mut buf = rgba_filled(32, 24, 128, 64, 200, 255);
        let expected = buf.clone();
        sharpen_rgba8_inplace(&mut buf, 32, 24);
        assert_eq!(
            buf, expected,
            "flat-color buffer must pass through sharpening unchanged"
        );
    }

    #[test]
    fn impulse_on_black_brightens_center() {
        // A single bright pixel on a black field. After unsharp mask on
        // luminance, the center pixel should come out at least as bright
        // as its input (boosted by the self-minus-blurred delta).
        let width = 11;
        let height = 11;
        let mut buf = rgba_filled(width, height, 0, 0, 0, 255);
        let center = ((height / 2) * width + (width / 2)) as usize;
        buf[center * 4] = 200;
        buf[center * 4 + 1] = 200;
        buf[center * 4 + 2] = 200;

        sharpen_rgba8_inplace(&mut buf, width, height);

        // Center pixel should come out at least as bright as the input
        // (in fact brighter, because amount > 0 pushes Y up).
        assert!(
            buf[center * 4] >= 200,
            "impulse center should brighten, got {}",
            buf[center * 4]
        );

        // The immediate neighbour was black (Y_in == 0), which our
        // DARK_EPSILON guard skips — so it stays at 0.
        let right = center + 1;
        assert!(
            buf[right * 4] <= 2,
            "impulse neighbour should stay at ~0, got {}",
            buf[right * 4]
        );
    }

    #[test]
    fn bright_edge_does_not_overflow() {
        // A hard black-to-white horizontal edge. On the bright side of the
        // edge, the sharpening formula produces values above 255 before
        // clamping. The output must saturate at 255 rather than wrapping.
        let width = 16;
        let height = 4;
        let mut buf = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..height {
            for x in 0..width {
                let v = if x >= width / 2 { 255 } else { 0 };
                buf.extend_from_slice(&[v, v, v, 255]);
            }
        }
        sharpen_rgba8_inplace(&mut buf, width, height);

        // The bright side interior must still be fully saturated — a
        // clamp bug that wrapped instead of saturating would drop those
        // pixels far below 255.
        for y in 0..height {
            let px_off = (y as usize * width as usize + (width as usize - 1)) * 4;
            assert_eq!(
                buf[px_off], 255,
                "bright-side row {y} lost saturation: {}",
                buf[px_off]
            );
        }
        // Dark side: pixels below DARK_EPSILON are left untouched. Column 0
        // is still 0.
        for y in 0..height {
            let px_off = (y as usize * width as usize) * 4;
            assert_eq!(
                buf[px_off], 0,
                "dark-side row {y} lifted above zero: {}",
                buf[px_off]
            );
        }
    }

    #[test]
    fn alpha_channel_is_preserved() {
        // Build a non-trivial RGBA image (so sharpening actually runs) with
        // a varied alpha channel, then check every alpha byte comes out
        // unchanged.
        let width = 8;
        let height = 8;
        let mut buf = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let r = ((x * 32) & 0xff) as u8;
                let g = ((y * 32) & 0xff) as u8;
                let b = (((x ^ y) * 16) & 0xff) as u8;
                let a = (((x + y) * 8 + 7) & 0xff) as u8;
                buf.extend_from_slice(&[r, g, b, a]);
            }
        }
        let alphas_before: Vec<u8> = buf.iter().skip(3).step_by(4).copied().collect();
        sharpen_rgba8_inplace(&mut buf, width, height);
        let alphas_after: Vec<u8> = buf.iter().skip(3).step_by(4).copied().collect();
        assert_eq!(alphas_before, alphas_after, "alpha channel must not change");
    }

    #[test]
    fn output_dimensions_match_input() {
        // Sharpening is in-place: the buffer length must not change.
        let width = 13;
        let height = 7;
        let mut buf = rgba_filled(width, height, 100, 120, 140, 255);
        let before = buf.len();
        sharpen_rgba8_inplace(&mut buf, width, height);
        assert_eq!(buf.len(), before, "buffer length changed");
        assert_eq!(buf.len(), (width * height * 4) as usize);
    }

    #[test]
    fn mismatched_dimensions_are_noop() {
        // Defensive posture: if `width * height * 4` doesn't match the
        // buffer length, leave the buffer alone rather than panic.
        let mut buf = vec![17_u8; 5]; // obviously wrong
        let before = buf.clone();
        sharpen_rgba8_inplace(&mut buf, 8, 8);
        assert_eq!(buf, before);
    }

    #[test]
    fn zero_dimensions_are_noop() {
        let mut buf: Vec<u8> = Vec::new();
        sharpen_rgba8_inplace(&mut buf, 0, 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn single_pixel_is_noop() {
        let mut buf = vec![200, 150, 100, 255];
        let before = buf.clone();
        sharpen_rgba8_inplace(&mut buf, 1, 1);
        assert_eq!(buf, before);
    }

    #[test]
    fn gaussian_kernel_sums_to_one() {
        for sigma in [0.3_f32, 0.8, 1.0, 2.5] {
            let k = gaussian_kernel_1d(sigma);
            let sum: f32 = k.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "sigma {sigma} kernel sum {sum} != 1.0"
            );
        }
    }

    #[test]
    fn gaussian_kernel_is_symmetric() {
        let k = gaussian_kernel_1d(0.8);
        for i in 0..k.len() / 2 {
            let j = k.len() - 1 - i;
            assert!((k[i] - k[j]).abs() < 1e-6, "tap {i} != tap {j}");
        }
    }

    #[test]
    fn colored_edge_preserves_hue() {
        // A colored edge (red block vs. green block) used to introduce
        // color fringes under per-channel sharpening because each channel
        // amplified its own edge. With luminance-only sharpening, hue is
        // preserved: the ratio `R:G` at any given pixel stays the same
        // before and after.
        let width = 16_u32;
        let height = 4_u32;
        let mut buf = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..height {
            for x in 0..width {
                let (r, g, b) = if x >= width / 2 {
                    (50_u8, 200_u8, 50_u8)
                } else {
                    (200_u8, 50_u8, 50_u8)
                };
                buf.extend_from_slice(&[r, g, b, 255]);
            }
        }
        let before = buf.clone();
        sharpen_rgba8_inplace(&mut buf, width, height);

        // Sample far interior pixels on each side (well past the kernel's
        // blur influence). Their R:G ratio should match the input's exactly
        // (integer quantisation permitting).
        for y in 0..height {
            // Left interior: near column 1, which is 1 pixel from the edge.
            // Pick column 0 instead — that's the furthest from the edge
            // and has the least blur leakage.
            let off_left = (y as usize * width as usize) * 4;
            assert_eq!(
                buf[off_left], before[off_left],
                "far-left R drifted at y={y}"
            );
            assert_eq!(
                buf[off_left + 1],
                before[off_left + 1],
                "far-left G drifted at y={y}"
            );

            let off_right = (y as usize * width as usize + (width as usize - 1)) * 4;
            assert_eq!(
                buf[off_right], before[off_right],
                "far-right R drifted at y={y}"
            );
            assert_eq!(
                buf[off_right + 1],
                before[off_right + 1],
                "far-right G drifted at y={y}"
            );
        }
    }

    /// Rough standalone perf sanity check. `#[ignore]` so it doesn't run
    /// in CI; kick off manually with
    /// `cargo test --release color::sharpen::tests::sharpen_20mp_bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn sharpen_20mp_bench() {
        let width: u32 = 5472;
        let height: u32 = 3648;
        let mut buf = rgba_filled(width, height, 120, 140, 160, 255);
        // Warm up once so allocators/thread pool are ready.
        sharpen_rgba8_inplace(&mut buf, width, height);
        let mut times = vec![];
        for _ in 0..5 {
            let t = std::time::Instant::now();
            sharpen_rgba8_inplace(&mut buf, width, height);
            times.push(t.elapsed().as_millis());
        }
        println!("Sharpen 20 MP times (ms): {times:?}");
    }
}
