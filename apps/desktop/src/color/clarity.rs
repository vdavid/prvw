//! Local contrast ("clarity") enhancement for RAW output.
//!
//! Same separable-Gaussian unsharp-mask idea [`super::sharpen`] uses, but
//! at a much larger radius. Capture sharpening (`σ ≈ 0.8` px) works at the
//! pixel-edge level, so its effect is only visible at 100 % zoom; at
//! fit-to-window zoom the display downsample averages it out. Clarity
//! (`σ ≈ 10` px) works on midtone features — shape silhouettes, textures,
//! the mid-frequency content — which survives display downscaling. That's
//! why Lightroom's "Clarity" and Affinity's "Detail Refinement" slider
//! make photos look noticeably crisper at ALL zoom levels.
//!
//! Luminance-only, edge-replication, rayon row-parallel — all the same
//! invariants as capture sharpening (see `sharpen.rs` for the full
//! rationale on luminance-only vs. per-channel, Rec.709 weights,
//! post-ICC slot, etc.).
//!
//! ## Defaults
//!
//! - **Radius σ = 10 px.** Midtone features without smearing shape
//!   outlines. Affinity's "Detail Refinement" at its default "25 %"
//!   appears to sit around σ = 20-25 px; we stay slightly more
//!   conservative so halos don't appear on high-contrast edges.
//! - **Amount = 0.40.** Moderate.
//!
//! Users tune both via Settings → RAW → Detail.
//!
//! ## Pipeline position
//!
//! Runs in display-space RGBA8 / RGBA16F **before** capture sharpening,
//! so the ordering is clarity (mid-frequency lift) → capture sharpening
//! (fine edges). Both operate on luminance only; their effects compose
//! cleanly.
//!
//! ## Perf: downsample-blur-upsample (Phase 6.4)
//!
//! A direct separable Gaussian at σ = 10 is a 61-tap kernel. Two passes
//! across a 20 MP plane ≈ 2.4 B FMAs → ~144 ms on Apple Silicon release.
//!
//! The signal clarity extracts is, by definition, low-frequency
//! (large-σ Gaussian = low-pass). Sampling it at 1/4 resolution loses
//! almost nothing visually, and both down/upsample are linear-cost. At
//! the downsampled resolution σ' = σ / K = 2.5, so the kernel shrinks
//! to ~15 taps AND the plane shrinks to w × h / 16. Net blur cost:
//! (15 / 61) × (1 / 16) ≈ 1.5 % of the original. The down and up passes
//! add small, linear overheads. Expected total ~40 ms, ~3.5× faster.
//!
//! We switch paths at `SIGMA_DIRECT_THRESHOLD`: for small σ the direct
//! convolution is cheap enough that the round-trip cost isn't worth it,
//! and the blur-at-reduced-res approximation starts costing visible
//! precision. Above the threshold we downsample → blur → upsample; at
//! or below we go straight through the same routines `sharpen.rs` uses.
//!
//! ## Edge handling for non-multiple-of-4 dimensions
//!
//! We pad the luma plane up to the nearest multiple of K by edge
//! replication before box-averaging. The padded samples contribute
//! to the last downsampled row/column the same way the edge pixel
//! itself does, which matches the "clamp-to-edge" convention used
//! throughout the rest of the color pipeline. After bilinear upsample
//! we simply truncate back to the original `w × h`.
//!
//! ## Why share `sharpen.rs` internals rather than duplicate
//!
//! The Gaussian kernel builder, the separable blur, and the luma
//! extractor are all identical — having two copies would drift. We
//! expose them as `pub(super)` from `sharpen.rs` so clarity can call
//! them directly. The downsample / upsample helpers and the combine
//! pass are clarity-specific and live here.

use half::f16;
use rayon::prelude::*;

use super::sharpen;

/// Default radius for the local-contrast Gaussian, in pixels. Matches the
/// Settings → RAW → Detail slider's default position.
pub const DEFAULT_RADIUS: f32 = 10.0;
/// Default unsharp-mask amount for the local-contrast pass. 0.0 = no
/// effect, 1.0 = aggressive.
pub const DEFAULT_AMOUNT: f32 = 0.40;

/// Downsample factor for the fast path. 4 is the standard trade: the
/// blurred signal has next to no content above the Nyquist of a /4
/// grid, and the post-upsample bilinear interpolation smooths out the
/// remaining aliasing invisibly.
const DOWNSAMPLE_FACTOR: usize = 4;

/// σ below this value still goes through the direct-convolution path.
/// At σ = 4 the kernel is 25 taps, which is cheap enough that the
/// overhead of the down/up passes (allocating two extra planes,
/// box-averaging, bilinear upsample) isn't a net win. Above the
/// threshold the direct kernel grows quadratically with σ while the
/// downsampled one stays small, so the fast path pulls ahead fast.
const SIGMA_DIRECT_THRESHOLD: f32 = 4.0;

/// Minimum pixel count to take the downsample fast path. Below this the
/// direct convolution is already negligible (well under a millisecond),
/// and the overhead of the extra allocations + bilinear upsample isn't
/// worth it. A 1024 × 1024 plane is the break-even on Apple Silicon in
/// informal timing; we set the gate slightly below to cover phone-
/// sized thumbnails and panel previews.
const MIN_PIXELS_FOR_FAST_PATH: usize = 1 << 20; // 1 MiB of pixels ≈ 1 MP

/// Apply a local-contrast pass to an RGBA8 buffer. `radius` is the
/// Gaussian σ in pixels; `amount` scales the unsharp-mask contribution.
/// Production callers pass [`DEFAULT_RADIUS`] / [`DEFAULT_AMOUNT`]
/// unless the user has moved the Settings → RAW → Detail sliders.
pub fn apply_clarity_rgba8_inplace_with(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    radius: f32,
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
        return;
    }
    if amount == 0.0 {
        return;
    }

    // Small σ or small image: cheaper to do a direct convolution than
    // pay the down/upsample round-trip. The fast path's approximation
    // also grows noticeable on tiny images (< ~1 MP) where individual
    // pixels weigh more on the bilinear interpolation error.
    if radius < SIGMA_DIRECT_THRESHOLD || pixels < MIN_PIXELS_FOR_FAST_PATH {
        sharpen::sharpen_rgba8_inplace_with(rgba, width, height, radius, amount);
        return;
    }

    let luma_in = sharpen::compute_luma(rgba);
    let blurred = blur_via_downsample(&luma_in, width, height, radius);

    rgba.par_chunks_exact_mut(4)
        .zip(luma_in.par_iter())
        .zip(blurred.par_iter())
        .for_each(|((px, &y_in), &y_blurred)| {
            if y_in < sharpen::DARK_EPSILON {
                return;
            }
            let y_out = y_in + (y_in - y_blurred) * amount;
            let scale = if y_out <= 0.0 { 0.0 } else { y_out / y_in };
            let r = px[0] as f32 * scale;
            let g = px[1] as f32 * scale;
            let b = px[2] as f32 * scale;
            px[0] = sharpen::f32_to_u8(r);
            px[1] = sharpen::f32_to_u8(g);
            px[2] = sharpen::f32_to_u8(b);
            // px[3] (alpha) intentionally untouched
        });
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
    if width == 0 || height == 0 {
        return;
    }
    let pixels = (width as usize) * (height as usize);
    if rgba.len() != pixels * 4 {
        return;
    }
    if pixels < 2 {
        return;
    }
    if amount == 0.0 {
        return;
    }

    if radius < SIGMA_DIRECT_THRESHOLD || pixels < MIN_PIXELS_FOR_FAST_PATH {
        sharpen::sharpen_rgba16f_inplace_with(rgba, width, height, radius, amount);
        return;
    }

    // Decode Y from f16 RGB into an f32 plane — no clamp, HDR
    // highlights above 1.0 must survive the blur.
    let mut luma_in = vec![0.0_f32; pixels];
    luma_in
        .par_iter_mut()
        .zip(rgba.par_chunks_exact(4))
        .for_each(|(slot, px)| {
            let r = f16::from_bits(px[0]).to_f32();
            let g = f16::from_bits(px[1]).to_f32();
            let b = f16::from_bits(px[2]).to_f32();
            *slot = sharpen::LUMA_R * r + sharpen::LUMA_G * g + sharpen::LUMA_B * b;
        });

    let blurred = blur_via_downsample(&luma_in, width, height, radius);

    rgba.par_chunks_exact_mut(4)
        .zip(luma_in.par_iter())
        .zip(blurred.par_iter())
        .for_each(|((px, &y_in), &y_blurred)| {
            if y_in < sharpen::DARK_EPSILON {
                return;
            }
            let y_out = y_in + (y_in - y_blurred) * amount;
            let scale = if y_out <= 0.0 { 0.0 } else { y_out / y_in };
            let r = f16::from_bits(px[0]).to_f32() * scale;
            let g = f16::from_bits(px[1]).to_f32() * scale;
            let b = f16::from_bits(px[2]).to_f32() * scale;
            px[0] = f16::from_f32(r).to_bits();
            px[1] = f16::from_f32(g).to_bits();
            px[2] = f16::from_f32(b).to_bits();
            // px[3] (alpha) intentionally untouched
        });
}

/// Downsample → Gaussian blur → upsample pipeline on a single-channel
/// f32 luma plane. Output dimensions match input dimensions.
///
/// At σ = 10 the downsampled σ' = 2.5, so the kernel shrinks from ~61
/// taps to ~15 AND the plane shrinks by 16×. Both linear helpers
/// (box-average downsample, bilinear upsample) are cheap.
fn blur_via_downsample(luma: &[f32], width: u32, height: u32, sigma: f32) -> Vec<f32> {
    let w = width as usize;
    let h = height as usize;
    let k = DOWNSAMPLE_FACTOR;

    // Pad dimensions up to a multiple of K by edge replication. The
    // padded planes own a separate buffer so we don't mutate `luma`.
    let w_padded = w.div_ceil(k) * k;
    let h_padded = h.div_ceil(k) * k;
    let padded = if w_padded == w && h_padded == h {
        None
    } else {
        Some(pad_replicate(luma, w, h, w_padded, h_padded))
    };
    let src: &[f32] = padded.as_deref().unwrap_or(luma);

    // Box-average downsample to (w/K, h/K).
    let w_small = w_padded / k;
    let h_small = h_padded / k;
    let small = downsample_box(src, w_padded, h_padded, w_small, h_small, k);

    // Separable Gaussian blur at σ / K.
    let sigma_small = sigma / k as f32;
    let kernel = sharpen::gaussian_kernel_1d(sigma_small);
    let radius = kernel.len() / 2;
    let mut scratch = vec![0.0_f32; w_small * h_small];
    let mut blurred_small = vec![0.0_f32; w_small * h_small];
    sharpen::blur_horizontal(
        &small,
        &mut scratch,
        w_small as u32,
        h_small as u32,
        &kernel,
        radius,
    );
    sharpen::blur_vertical(
        &scratch,
        &mut blurred_small,
        w_small as u32,
        h_small as u32,
        &kernel,
        radius,
    );

    // Bilinear upsample back to the padded full resolution.
    let blurred_padded = upsample_bilinear(&blurred_small, w_small, h_small, w_padded, h_padded);

    // Crop to the original `w × h`. If padded == unpadded, this is a move.
    if w_padded == w && h_padded == h {
        blurred_padded
    } else {
        crop(&blurred_padded, w_padded, w, h)
    }
}

/// Edge-replicate a plane from `(w, h)` to `(w_padded, h_padded)`.
/// Callers only ever ask for pads up to K-1 in each direction so the
/// allocation cost is negligible.
fn pad_replicate(src: &[f32], w: usize, h: usize, w_padded: usize, h_padded: usize) -> Vec<f32> {
    let mut dst = vec![0.0_f32; w_padded * h_padded];
    dst.par_chunks_exact_mut(w_padded)
        .enumerate()
        .for_each(|(y, row)| {
            let src_y = y.min(h - 1);
            let src_row = &src[src_y * w..src_y * w + w];
            row[..w].copy_from_slice(src_row);
            // Right-side replication: reuse the last input column.
            let edge = src_row[w - 1];
            for slot in row.iter_mut().take(w_padded).skip(w) {
                *slot = edge;
            }
        });
    dst
}

/// Crop a `(w_padded, h_padded)` plane down to `(w, h)`. Assumes
/// `w <= w_padded` and `h <= h_padded`.
fn crop(src: &[f32], w_padded: usize, w: usize, h: usize) -> Vec<f32> {
    let mut dst = vec![0.0_f32; w * h];
    dst.par_chunks_exact_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            let src_row = &src[y * w_padded..y * w_padded + w];
            row.copy_from_slice(src_row);
        });
    dst
}

/// Box-filter downsample: each output pixel is the mean of a K×K tile
/// of input pixels. `w_in` and `h_in` must be multiples of K.
fn downsample_box(
    src: &[f32],
    w_in: usize,
    _h_in: usize,
    w_out: usize,
    h_out: usize,
    k: usize,
) -> Vec<f32> {
    let mut dst = vec![0.0_f32; w_out * h_out];
    let inv_area = 1.0 / (k * k) as f32;
    dst.par_chunks_exact_mut(w_out)
        .enumerate()
        .for_each(|(y_out, row)| {
            let y_base = y_out * k;
            for (x_out, slot) in row.iter_mut().enumerate() {
                let x_base = x_out * k;
                let mut acc = 0.0_f32;
                for dy in 0..k {
                    let src_row_off = (y_base + dy) * w_in + x_base;
                    for dx in 0..k {
                        acc += src[src_row_off + dx];
                    }
                }
                *slot = acc * inv_area;
            }
        });
    dst
}

/// Bilinear upsample from `(w_in, h_in)` to `(w_out, h_out)`. Sample
/// positions use the "pixel centers" convention: the output pixel at
/// `(x_out, y_out)` maps to source coordinates
/// `((x_out + 0.5) * w_in / w_out - 0.5, ...)`. This keeps the
/// center-of-image aligned between the two grids, which matters when
/// the up/down factor isn't a perfect integer — not the case here
/// (`w_out = k * w_in`) but the convention doesn't cost extra.
fn upsample_bilinear(
    src: &[f32],
    w_in: usize,
    h_in: usize,
    w_out: usize,
    h_out: usize,
) -> Vec<f32> {
    let mut dst = vec![0.0_f32; w_out * h_out];
    let sx_scale = w_in as f32 / w_out as f32;
    let sy_scale = h_in as f32 / h_out as f32;
    dst.par_chunks_exact_mut(w_out)
        .enumerate()
        .for_each(|(y_out, row)| {
            let sy = (y_out as f32 + 0.5) * sy_scale - 0.5;
            let sy_floor = sy.floor();
            let ty = sy - sy_floor;
            let y0 = (sy_floor as isize).clamp(0, (h_in - 1) as isize) as usize;
            let y1 = ((sy_floor as isize) + 1).clamp(0, (h_in - 1) as isize) as usize;
            let row0 = &src[y0 * w_in..y0 * w_in + w_in];
            let row1 = &src[y1 * w_in..y1 * w_in + w_in];
            for (x_out, slot) in row.iter_mut().enumerate() {
                let sx = (x_out as f32 + 0.5) * sx_scale - 0.5;
                let sx_floor = sx.floor();
                let tx = sx - sx_floor;
                let x0 = (sx_floor as isize).clamp(0, (w_in - 1) as isize) as usize;
                let x1 = ((sx_floor as isize) + 1).clamp(0, (w_in - 1) as isize) as usize;
                let a = row0[x0] * (1.0 - tx) + row0[x1] * tx;
                let b = row1[x0] * (1.0 - tx) + row1[x1] * tx;
                *slot = a * (1.0 - ty) + b * ty;
            }
        });
    dst
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

    /// Non-multiple-of-4 dimensions must still run without panicking and
    /// leave flat pixels unchanged. Catches off-by-one errors in the
    /// padding / crop step.
    #[test]
    fn non_multiple_of_four_dims_ok() {
        // 17 × 13 is coprime with 4 in both directions.
        let mut rgba: Vec<u8> = (0..17 * 13).flat_map(|_| [100u8, 100, 100, 255]).collect();
        let expected = rgba.clone();
        apply_clarity_rgba8_inplace_with(&mut rgba, 17, 13, DEFAULT_RADIUS, DEFAULT_AMOUNT);
        assert_eq!(rgba, expected);
        assert_eq!(rgba.len(), 17 * 13 * 4);
    }

    /// The downsample path for σ ≥ threshold must agree visually with a
    /// direct convolution on a gently-varying pattern. The blurred
    /// signal is low-frequency by construction, so the down/up round
    /// trip only shuffles pixels by a couple of gray levels.
    #[test]
    fn large_sigma_matches_direct_within_tolerance() {
        // A radial gradient — plenty of low-frequency content, no hard
        // edges that would expose the bilinear upsample. Size is above
        // MIN_PIXELS_FOR_FAST_PATH so the downsample path fires.
        let width = 1024_u32;
        let height = 1024_u32;
        let pixels = (width * height) as usize;
        let mut rgba_fast = Vec::with_capacity(pixels * 4);
        let cx = width as f32 / 2.0;
        let cy = height as f32 / 2.0;
        let max_r = (cx * cx + cy * cy).sqrt();
        for y in 0..height {
            for x in 0..width {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let r = (dx * dx + dy * dy).sqrt();
                let v = (255.0 * (1.0 - r / max_r)).clamp(0.0, 255.0) as u8;
                rgba_fast.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let mut rgba_direct = rgba_fast.clone();

        // σ = 10 triggers the downsample fast path in the clarity module.
        apply_clarity_rgba8_inplace_with(&mut rgba_fast, width, height, 10.0, 0.4);
        // Direct reference goes through sharpen.rs at the same σ.
        sharpen::sharpen_rgba8_inplace_with(&mut rgba_direct, width, height, 10.0, 0.4);

        // Tolerance: per-pixel diff ≤ 5 gray levels, average diff ≤ 1.
        // Low-frequency content across a 64×64 gradient: the bilinear
        // upsample of a /4-sampled blur is visually indistinguishable,
        // but the discrete box-average introduces a handful of pixels
        // near the steepest slope that diverge by a few gray levels.
        // Going below ~5 would require a more expensive upsample
        // filter for no visible gain.
        let mut max_diff: i32 = 0;
        let mut sum_diff: i64 = 0;
        let mut n: i64 = 0;
        for i in 0..pixels {
            let off = i * 4;
            for c in 0..3 {
                let d = (rgba_fast[off + c] as i32 - rgba_direct[off + c] as i32).abs();
                if d > max_diff {
                    max_diff = d;
                }
                sum_diff += d as i64;
                n += 1;
            }
        }
        let avg_diff = sum_diff as f64 / n as f64;
        assert!(
            max_diff <= 5,
            "fast path diverged from direct convolution by {max_diff} gray levels (max)"
        );
        assert!(
            avg_diff <= 1.0,
            "fast path average diff {avg_diff:.3} > 1.0 gray levels"
        );
    }

    /// Small σ takes the direct path and MUST match today's output
    /// exactly (no down/upsample).
    #[test]
    fn small_sigma_matches_direct_exactly() {
        let width = 16_u32;
        let height = 16_u32;
        let pixels = (width * height) as usize;
        let mut rgba_clarity = Vec::with_capacity(pixels * 4);
        for y in 0..height {
            for x in 0..width {
                let v = ((x * 16 + y * 8) & 0xff) as u8;
                rgba_clarity.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let mut rgba_sharpen = rgba_clarity.clone();

        // σ below the threshold must fall through to the direct path.
        apply_clarity_rgba8_inplace_with(&mut rgba_clarity, width, height, 2.0, 0.3);
        sharpen::sharpen_rgba8_inplace_with(&mut rgba_sharpen, width, height, 2.0, 0.3);

        assert_eq!(
            rgba_clarity, rgba_sharpen,
            "σ < SIGMA_DIRECT_THRESHOLD must produce identical bytes to the direct path"
        );
    }

    /// Rough standalone perf sanity check for the downsample fast path.
    /// `#[ignore]` so it doesn't run in CI; kick off manually with
    /// `cargo test --release color::clarity::tests::clarity_20mp_bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn clarity_20mp_bench() {
        let width: u32 = 5472;
        let height: u32 = 3648;
        let mut buf: Vec<u8> = (0..(width * height))
            .flat_map(|i| {
                let v = (i % 256) as u8;
                [v, v, v, 255]
            })
            .collect();
        // Warm up once.
        apply_clarity_rgba8_inplace_with(&mut buf, width, height, DEFAULT_RADIUS, DEFAULT_AMOUNT);
        let mut times = vec![];
        for _ in 0..5 {
            let t = std::time::Instant::now();
            apply_clarity_rgba8_inplace_with(
                &mut buf,
                width,
                height,
                DEFAULT_RADIUS,
                DEFAULT_AMOUNT,
            );
            times.push(t.elapsed().as_millis());
        }
        println!("Clarity 20 MP times (ms): {times:?}");
    }
}
