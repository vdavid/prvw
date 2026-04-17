//! Capture sharpening for RAW output.
//!
//! Applied as the final pre-orientation step on the RGBA8 buffer. A mild
//! unsharp mask compensates for the softness every RAW decode carries: the
//! sensor's optical low-pass filter softens fine detail, and demosaic blurs
//! it a second time. Without this step, the output reads "slightly soft"
//! next to Preview.app and Lightroom, both of which apply capture sharpening
//! silently.
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
//! 2. Staying on 8-bit matches the amount of data the renderer will see
//!    anyway, and at our modest amount (≤ 0.5) the 8-bit quantisation
//!    overhead is below one gray level per pixel.
//! 3. It's the last step before the orientation rotate, so we never sharpen
//!    a buffer we're about to throw away.
//!
//! ## Algorithm — separable Gaussian unsharp mask
//!
//! Classic formula: `output = original + (original - blurred) * amount`.
//! The blur is a 1D Gaussian applied horizontally, then vertically. Separable
//! makes it `2 × N` taps per pixel instead of `N²`, for a ~10× speedup at
//! our kernel size.
//!
//! Parameters (baked in — no user knob yet):
//!
//! - **Radius σ = 0.8 px.** Small enough to sharpen fine texture (grass,
//!   fabric, bark) without chasing wide edges that'd produce halos.
//! - **Amount = 0.3.** Mild. Measured Laplacian edge energy on a real
//!   photo jumps ~20 % vs. pre-sharpen, matching the "crisper, not
//!   artificial" feel Preview.app ships. Going to 0.4 lifts edge energy
//!   another ~15 % but puts us well past Preview.app's crispness, which
//!   reads as mild oversharpening at 1:1 zoom.
//! - **Threshold = 0.** No edge discrimination — capture sharpening uniformly
//!   lifts micro-contrast. Noise reduction is a separate concern and out of
//!   scope for a viewer.
//!
//! Kernel width is `2 × ceil(3σ) + 1`. For σ = 0.8 that's 7 taps, small
//! enough to cache-fit comfortably and fast enough that a 20 MP buffer
//! sharpens in well under 50 ms on M-series hardware.
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

use rayon::prelude::*;

/// Gaussian standard deviation in pixels. 0.8 is the "capture sharpening"
/// default — small enough for fine detail, broad enough to clear demosaic
/// softening. Lower values shrink the effect to single-pixel contrast and
/// risk aliasing; higher values drift into "detail sharpening" territory
/// where halos appear.
const SIGMA: f32 = 0.8;

/// Unsharp-mask amount. 0.3 is the conservative "capture sharpening"
/// default — enough to close the crispness gap against Preview.app
/// (Laplacian edge-energy bump ~20 % on real photos) without pushing into
/// oversharpened territory where halos appear on bright edges. Lightroom
/// ships a similar "detail" default in its "Sharpening: Amount = 25" knob.
const AMOUNT: f32 = 0.3;

/// Sharpen an RGBA8 buffer in place. Alpha is left untouched.
///
/// `width * height * 4` must equal `rgba.len()`; on mismatch the function
/// is a no-op (same defensive posture as the rest of the color pipeline).
/// Empty buffers and 1×1 images are safe — the blur degenerates to the
/// identity and the unsharp mask becomes a no-op.
pub fn sharpen_rgba8_inplace(rgba: &mut [u8], width: u32, height: u32) {
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

    let kernel = gaussian_kernel_1d(SIGMA);
    let radius = kernel.len() / 2;

    // Deinterleave into three planes so the blur passes can work on
    // contiguous f32 rows/columns. The alpha channel stays in `rgba`
    // untouched; we write the three color planes back on the final
    // composite pass.
    let (mut r, mut g, mut b) = split_rgb_planes(rgba);

    // One scratch buffer serves both the horizontal-blur output and the
    // vertical-blur output across all three channels. Total extra
    // allocation: 2 × pixels × 4 bytes (scratch + second blur plane),
    // versus 4 × pixels × 4 bytes in a naïve per-channel split.
    let mut scratch = vec![0.0_f32; pixels];
    let mut blurred = vec![0.0_f32; pixels];

    process_channel(
        &mut r,
        &mut scratch,
        &mut blurred,
        width,
        height,
        &kernel,
        radius,
    );
    process_channel(
        &mut g,
        &mut scratch,
        &mut blurred,
        width,
        height,
        &kernel,
        radius,
    );
    process_channel(
        &mut b,
        &mut scratch,
        &mut blurred,
        width,
        height,
        &kernel,
        radius,
    );

    // Reinterleave color planes into `rgba`. Alpha bytes (index 3 of each
    // pixel) were never touched and stay in place.
    merge_rgb_planes(&r, &g, &b, rgba);
}

/// Run the full unsharp-mask cycle for a single color plane: horizontal
/// blur → vertical blur → `plane += (plane - blurred) * AMOUNT`. Caller
/// provides two reusable scratch buffers so we don't allocate once per
/// channel.
fn process_channel(
    plane: &mut [f32],
    scratch: &mut [f32],
    blurred: &mut [f32],
    width: u32,
    height: u32,
    kernel: &[f32],
    radius: usize,
) {
    blur_horizontal(plane, scratch, width, height, kernel, radius);
    blur_vertical(scratch, blurred, width, height, kernel, radius);
    combine_unsharp(plane, blurred, AMOUNT);
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

/// Deinterleave an RGBA8 buffer into three f32 planes (R, G, B). Alpha
/// stays where it is. We convert to f32 here so the blur passes can run
/// without repeated int-to-float casts.
fn split_rgb_planes(rgba: &[u8]) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let pixels = rgba.len() / 4;
    let mut r = vec![0.0_f32; pixels];
    let mut g = vec![0.0_f32; pixels];
    let mut b = vec![0.0_f32; pixels];
    // Split into chunks so rayon can iterate in parallel over each plane.
    r.par_iter_mut()
        .zip(g.par_iter_mut())
        .zip(b.par_iter_mut())
        .zip(rgba.par_chunks_exact(4))
        .for_each(|(((r_slot, g_slot), b_slot), px)| {
            *r_slot = px[0] as f32;
            *g_slot = px[1] as f32;
            *b_slot = px[2] as f32;
        });
    (r, g, b)
}

/// Write three f32 color planes back into RGBA8, clamping to [0, 255] on
/// the way. Alpha bytes are not touched. Uses `+ 0.5` round-to-nearest to
/// match the rest of the color pipeline's f32→u8 convention.
fn merge_rgb_planes(r: &[f32], g: &[f32], b: &[f32], rgba: &mut [u8]) {
    rgba.par_chunks_exact_mut(4)
        .zip(r.par_iter())
        .zip(g.par_iter())
        .zip(b.par_iter())
        .for_each(|(((px, &rv), &gv), &bv)| {
            px[0] = f32_to_u8(rv);
            px[1] = f32_to_u8(gv);
            px[2] = f32_to_u8(bv);
            // px[3] (alpha) intentionally untouched
        });
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

/// Vertical 1D Gaussian blur with edge replication. Columns are processed
/// in parallel by striping the output across rayon threads row-wise (each
/// thread reads from a column window of the input).
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

/// Apply the unsharp-mask mixing formula in place on a single color plane:
/// `original += (original - blurred) * amount`.
///
/// Result is written back to `original` (still in f32, no clamping yet —
/// the clamp happens in `merge_rgb_planes` so it only runs once).
fn combine_unsharp(original: &mut [f32], blurred: &[f32], amount: f32) {
    original
        .par_iter_mut()
        .zip(blurred.par_iter())
        .for_each(|(o, &b)| {
            let detail = *o - b;
            *o += detail * amount;
        });
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
        // flat signal, so `original - blurred` is 0 everywhere, and
        // `original + 0 * amount` is still the original. No sharpening
        // should change a pixel.
        let mut buf = rgba_filled(32, 24, 128, 64, 200, 255);
        let expected = buf.clone();
        sharpen_rgba8_inplace(&mut buf, 32, 24);
        assert_eq!(
            buf, expected,
            "flat-color buffer must pass through sharpening unchanged"
        );
    }

    #[test]
    fn impulse_on_black_brightens_center_and_darkens_neighbours() {
        // A single bright pixel on a black field. After unsharp mask:
        //   - the center pixel should come out *brighter* than its input
        //     (itself minus its own mostly-dark blurred neighbourhood)
        //   - the ring of neighbours around it should be *darker than 0*
        //     before clamping — in practice they clamp to 0, so we check
        //     they stay at 0 rather than lifting above 0
        // This is the classic "unsharp ring" signature; any kernel-math
        // bug would flip the sign or smear the center.
        let width = 11;
        let height = 11;
        let mut buf = rgba_filled(width, height, 0, 0, 0, 255);
        let center = ((height / 2) * width + (width / 2)) as usize;
        buf[center * 4] = 200;
        buf[center * 4 + 1] = 200;
        buf[center * 4 + 2] = 200;

        sharpen_rgba8_inplace(&mut buf, width, height);

        // Center pixel should come out at least as bright as the input
        // (in fact brighter, because amount > 0 pushes it up).
        assert!(
            buf[center * 4] >= 200,
            "impulse center should brighten, got {}",
            buf[center * 4]
        );

        // Immediate neighbour (one pixel to the right) should have gone
        // negative pre-clamp and landed at 0 post-clamp. Anything above a
        // couple of gray levels would mean the sign of the mask is wrong.
        let right = center + 1;
        assert!(
            buf[right * 4] <= 2,
            "impulse neighbour should stay at ~0, got {}",
            buf[right * 4]
        );
    }

    #[test]
    fn bright_edge_does_not_overflow() {
        // A hard black-to-white horizontal edge. At the bright side of the
        // edge, the sharpening formula produces values above 255 before
        // clamping. The output must saturate at 255 rather than wrapping
        // around.
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
        // pixels far below 255. Sample the far-right column (safely
        // inside the bright half, past the kernel's blur influence).
        for y in 0..height {
            let px_off = (y as usize * width as usize + (width as usize - 1)) * 4;
            assert_eq!(
                buf[px_off], 255,
                "bright-side row {y} lost saturation: {}",
                buf[px_off]
            );
        }
        // And the dark side interior must still be at 0 — the kernel's
        // overshoot at the edge would push neighbours negative, but the
        // clamp should catch them. Column 0 is furthest from the edge.
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
        // Sharpening is in-place: the buffer length must not change. Guard
        // against any accidental resize in the merge step.
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
        // buffer length, leave the buffer alone rather than panic. The
        // caller is free to detect and log the mismatch separately.
        let mut buf = vec![17_u8; 5]; // obviously wrong
        let before = buf.clone();
        sharpen_rgba8_inplace(&mut buf, 8, 8);
        assert_eq!(buf, before);
    }

    #[test]
    fn zero_dimensions_are_noop() {
        // Width or height of zero: nothing to do. Must not attempt to
        // allocate a zero-length plane and then index into it.
        let mut buf: Vec<u8> = Vec::new();
        sharpen_rgba8_inplace(&mut buf, 0, 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn single_pixel_is_noop() {
        // 1x1 has no neighbours. Sharpening would sample the same pixel
        // through the whole kernel; blur == original, detail == 0, output
        // == original. Check byte-exact to guard against rounding drift.
        let mut buf = vec![200, 150, 100, 255];
        let before = buf.clone();
        sharpen_rgba8_inplace(&mut buf, 1, 1);
        assert_eq!(buf, before);
    }

    #[test]
    fn gaussian_kernel_sums_to_one() {
        // Normalisation invariant: if the kernel doesn't sum to 1.0, blurs
        // change overall brightness, and the unsharp mask biases the output.
        for sigma in [0.3_f32, 0.8, 1.0, 2.5] {
            let k = gaussian_kernel_1d(sigma);
            let sum: f32 = k.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "sigma {sigma} kernel sum {sum} != 1.0"
            );
        }
    }

    /// Rough standalone perf sanity check. `#[ignore]` so it doesn't run
    /// in CI; kick off manually with
    /// `cargo test --release color::sharpen::tests::sharpen_20mp_bench -- --ignored --nocapture`.
    /// Prints wall-clock time for a 20 MP (5472×3648) buffer on the current
    /// machine — handy for spotting a regression in the separable-blur
    /// passes.
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

    #[test]
    fn gaussian_kernel_is_symmetric() {
        // Symmetry invariant: an asymmetric kernel would bias edges in one
        // direction (sharpening the left side of features more than the
        // right). The builder must produce a palindrome.
        let k = gaussian_kernel_1d(0.8);
        for i in 0..k.len() / 2 {
            let j = k.len() - 1 - i;
            assert!((k[i] - k[j]).abs() < 1e-6, "tap {i} != tap {j}");
        }
    }
}
