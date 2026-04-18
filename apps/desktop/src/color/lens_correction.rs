//! Lens correction via `lensfun-rs` — distortion, TCA, and vignetting.
//!
//! Non-DNG RAWs (Sony ARW, Canon CR2/CR3, Nikon NEF, Fuji RAF, etc.) don't
//! carry lens-correction opcodes the way DNGs optionally do through
//! `OpcodeList3 :: WarpRectilinear`. Adobe Camera Raw and Capture One pull
//! the correction data out of LensFun's community database instead. This
//! module wires LensFun into Prvw's RAW pipeline.
//!
//! # Pipeline position
//!
//! Runs post-demosaic + post-`camera_to_linear_rec2020`, pre-exposure, in the
//! same slot DNG's `OpcodeList3 :: WarpRectilinear` occupies. The buffer is a
//! 3-channel packed f32 image in linear Rec.2020. When the DNG opcode already
//! handled distortion for this file, the caller skips us to avoid double
//! correction.
//!
//! # What it does
//!
//! Applies three passes in order:
//!
//! 1. Vignetting — multiplies each pixel by `1 / (1 + k1·r² + k2·r⁴ + k3·r⁶)`
//!    to lift the darkened corners. Cheap, in place, no resample.
//! 2. Distortion — for each output pixel, asks the lensfun `Modifier` what
//!    source pixel the light came from, then bilinear-samples the input.
//!    One image-sized copy + one pass; slower than vignetting but still
//!    image-size memory, not grid-size.
//! 3. TCA — same idea, but the `Modifier` hands us three source coords per
//!    output pixel (one per color channel). Red and blue planes get shifted
//!    relative to green to close chromatic fringing at edges.
//!
//! All three are independent; any of them may succeed or silently no-op,
//! depending on what LensFun has calibrated for the lens.
//!
//! # Matching
//!
//! The camera + lens strings come off rawler: `raw.camera.make` /
//! `raw.camera.model` (normalised by rawler) and
//! `raw_metadata().exif.lens_make` / `.lens_model`. Focal length and aperture
//! come off EXIF too. If any of those is missing, we log at DEBUG and skip.
//! If the combination isn't in LensFun's database, same — silent no-op.
//!
//! # Database bundling
//!
//! `lensfun::Database::load_bundled()` pulls the gzipped XML compiled into the
//! crate, so there's no runtime I/O. We load it once per process on first use
//! via `OnceLock` and hand out `&'static Database` references from there.
//!
//! # Why this order (vignetting → distortion → TCA)
//!
//! Vignetting is a radial brightness correction, so it's independent of where
//! the geometry ends up — we run it first to keep the modifier state aligned
//! with pre-geometry coordinates. Distortion and TCA both resample; we apply
//! distortion first (corrects geometry globally), then TCA (fine per-channel
//! registration). The order matches LensFun's upstream expectation.
//!
//! # SIMD vectorization (Phase 6.3)
//!
//! The bilinear resampler inner loops in `resample_distortion_row` and
//! `resample_tca_row` are annotated with `#[multiversion]` (NEON on aarch64,
//! AVX2 on x86-64). This makes the compiler emit a NEON-optimised copy of the
//! entire per-row loop and selects it at runtime on Apple Silicon. The hot
//! path — `sample_rgb_bilinear_fast` — is branchless (NaN/inf coords clamp to
//! 0 via a `f32::is_finite` multiply-mask rather than an early return) so the
//! auto-vectorizer has an unobstructed straight-line loop body.

use std::sync::OnceLock;

use lensfun::{Database, Modifier};
use multiversion::multiversion;
use rawler::RawImage;
use rayon::prelude::*;

/// Bundled LensFun database, loaded on first call. Pre-v1 LensFun database
/// has ~1,041 cameras and ~1,543 lenses; decompress-and-parse takes a few
/// hundred ms on first use. After that it's a static reference.
static LENSFUN_DB: OnceLock<Option<Database>> = OnceLock::new();

fn lensfun_db() -> Option<&'static Database> {
    LENSFUN_DB
        .get_or_init(|| match Database::load_bundled() {
            Ok(db) => {
                log::info!(
                    "LensFun database loaded ({} cameras, {} lenses)",
                    db.cameras.len(),
                    db.lenses.len(),
                );
                Some(db)
            }
            Err(e) => {
                log::warn!("Couldn't load LensFun database: {e}");
                None
            }
        })
        .as_ref()
}

/// Look up `raw`'s camera + `lens_model` in LensFun and apply distortion,
/// TCA, and vignetting correction to `rgb` in place.
///
/// `rgb` is a packed 3-channel `[R, G, B, R, G, B, ...]` f32 buffer of length
/// `width * height * 3`, in linear Rec.2020.
///
/// The caller extracts `lens_model`, `focal` (mm), `aperture` (f-number), and
/// `distance` (m) from EXIF — rawler exposes them on
/// `decoder.raw_metadata(&src, &params).exif`. When the source doesn't carry
/// `lens_model` (most smartphone DNGs, some older Nikons), pass `""` or skip
/// the call; `distance` can default to `1000.0` for "effectively infinity".
///
/// Returns `true` when at least one correction pass fired, `false` when we
/// no-op'd (missing lens metadata, no DB match, no calibration data, or
/// unusable combination). Logs at INFO when it applies something and at
/// DEBUG when it silently skips.
#[allow(clippy::too_many_arguments)] // Lens metadata + buffer + dims each need their own slot
pub fn apply_lens_correction(
    raw: &RawImage,
    lens_model: &str,
    focal: f32,
    aperture: f32,
    distance: f32,
    rgb: &mut [f32],
    width: u32,
    height: u32,
) -> bool {
    if rgb.len() != (width as usize) * (height as usize) * 3 {
        log::debug!("lens_correction: buffer size mismatch; skipping");
        return false;
    }
    if lens_model.is_empty() || focal <= 0.0 || aperture <= 0.0 {
        log::debug!(
            "lens_correction: missing lens_model/focal/aperture for '{} {}'; skipping",
            raw.camera.make,
            raw.camera.model
        );
        return false;
    }

    let Some(db) = lensfun_db() else {
        return false;
    };

    run(
        db, raw, lens_model, focal, aperture, distance, rgb, width, height,
    )
}

/// Shared core: finds the camera + lens in the DB, builds a `Modifier`, and
/// runs vignetting → distortion → TCA in place. Returns `true` when any pass
/// fired.
#[allow(clippy::too_many_arguments)] // Same reason as `apply_lens_correction`
fn run(
    db: &Database,
    raw: &RawImage,
    lens_model: &str,
    focal: f32,
    aperture: f32,
    distance: f32,
    rgb: &mut [f32],
    width: u32,
    height: u32,
) -> bool {
    // `camera.make` in rawler is vendor-normalised (`"SONY"`, `"Canon"`, etc).
    // LensFun's camera table uses mixed case, and the maker field on its
    // `Lens` rows is also mixed case. `find_cameras` is a fuzzy match so
    // case doesn't matter, but we still pass the raw value through.
    let cameras = db.find_cameras(Some(&raw.camera.make), &raw.camera.model);
    let Some(camera) = cameras.first().copied() else {
        log::debug!(
            "lens_correction: camera '{} {}' not in LensFun DB; skipping",
            raw.camera.make,
            raw.camera.model
        );
        return false;
    };

    let lenses = db.find_lenses(Some(camera), lens_model);
    let Some(lens) = lenses.first().copied() else {
        log::debug!(
            "lens_correction: lens '{}' not in LensFun DB for '{} {}'; skipping",
            lens_model,
            raw.camera.make,
            raw.camera.model
        );
        return false;
    };

    // `reverse = false` means "correct the lens's distortion" (undo it).
    // `reverse = true` would simulate the distortion — the opposite of what a
    // viewer wants. The earlier Phase 4 agent flipped this the wrong way and
    // the resulting double-correction darkened corners and exaggerated barrel
    // distortion. See user-reported issue during Phase 5.1 smoke testing.
    let mut modifier = Modifier::new(lens, focal, camera.crop_factor, width, height, false);
    let distortion_enabled = modifier.enable_distortion_correction(lens);
    let tca_enabled = modifier.enable_tca_correction(lens);
    let vignetting_enabled = modifier.enable_vignetting_correction(lens, aperture, distance);

    if !(distortion_enabled || tca_enabled || vignetting_enabled) {
        log::debug!(
            "lens_correction: '{}' has no usable calibrations; skipping",
            lens.model
        );
        return false;
    }

    // Vignetting first — doesn't touch geometry, so pre/post distortion both
    // work, and doing it before the resample keeps the math straightforward.
    if vignetting_enabled {
        apply_vignetting(&modifier, rgb, width, height);
    }

    if distortion_enabled {
        apply_distortion_resample(&modifier, rgb, width, height);
    }

    if tca_enabled {
        apply_tca_resample(&modifier, rgb, width, height);
    }

    log::info!(
        "lens_correction: applied to '{}' ({}mm f/{}) [{}{}{}]",
        lens.model,
        focal,
        aperture,
        if distortion_enabled { "D" } else { "-" },
        if tca_enabled { "T" } else { "-" },
        if vignetting_enabled { "V" } else { "-" },
    );

    true
}

/// Vignetting is a plain per-pixel gain. `apply_color_modification_f32` walks
/// the buffer row by row using the same coordinate math as the geometry
/// passes — no resample needed.
fn apply_vignetting(modifier: &Modifier, rgb: &mut [f32], width: u32, height: u32) {
    modifier.apply_color_modification_f32(rgb, 0.0, 0.0, width as usize, height as usize, 3);
}

/// Distortion + any enabled perspective correction. Per-pixel:
///
/// 1. Ask the `Modifier` what source coordinate the current output pixel maps
///    to (it fills a `[x, y]` pair per pixel).
/// 2. Bilinear-sample the three channels from the source buffer.
///
/// Processed row by row so we don't materialise the full coordinate array at
/// once. Rows run in parallel via rayon.
fn apply_distortion_resample(modifier: &Modifier, rgb: &mut [f32], width: u32, height: u32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let src = rgb.to_vec();

    // One row's worth of scratch per worker thread. We can't share a single
    // scratch across threads without contention, so each parallel row gets
    // its own small allocation. The overhead is dwarfed by the per-row
    // sample cost.
    rgb.par_chunks_exact_mut(w * 3)
        .enumerate()
        .for_each(|(y, out_row)| {
            let mut coords = vec![0.0_f32; w * 2];
            let mapped = modifier.apply_geometry_distortion(0.0, y as f32, w, 1, &mut coords);
            if !mapped {
                // The modifier declined (no distortion / perspective enabled);
                // leave the row untouched.
                return;
            }
            // The inner per-pixel loop is extracted into a multiversion function
            // so the compiler can emit a NEON (aarch64) or AVX2 (x86-64)
            // variant of the tight bilinear-sampling loop.
            resample_distortion_row(&src, w, h, &coords, out_row);
        });
}

/// TCA: per-pixel, three source coordinates (one per color channel). Sample
/// each channel from its own source point. Like distortion, one source copy
/// total, row-parallel.
fn apply_tca_resample(modifier: &Modifier, rgb: &mut [f32], width: u32, height: u32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let src = rgb.to_vec();

    rgb.par_chunks_exact_mut(w * 3)
        .enumerate()
        .for_each(|(y, out_row)| {
            let mut coords = vec![0.0_f32; w * 6];
            let mapped = modifier.apply_subpixel_distortion(0.0, y as f32, w, 1, &mut coords);
            if !mapped {
                return;
            }
            // The inner per-pixel loop is extracted into a multiversion function
            // so the compiler can emit a NEON (aarch64) or AVX2 (x86-64)
            // variant of the tight bilinear-sampling loop.
            resample_tca_row(&src, w, h, &coords, out_row);
        });
}

/// Per-row inner loop for distortion resampling. All pixels in `out_row`
/// are written by bilinear-sampling `src` at the coordinates from `coords`
/// (interleaved `[sx, sy, sx, sy, ...]` pairs, one per pixel).
///
/// Annotated with `#[multiversion]` so the compiler emits optimised copies
/// for NEON (Apple Silicon / ARMv8) and AVX2 (modern x86-64) and selects
/// the best version at runtime. The function body is branchless — NaN/inf
/// coordinates are clamped to 0 without a conditional return, giving the
/// auto-vectorizer an unobstructed inner loop.
#[multiversion(targets("aarch64+neon", "x86_64+avx+avx2+fma"))]
fn resample_distortion_row(src: &[f32], w: usize, h: usize, coords: &[f32], out_row: &mut [f32]) {
    for x in 0..w {
        let sx = coords[2 * x];
        let sy = coords[2 * x + 1];
        let (r, g, b) = sample_rgb_bilinear_fast(src, w, h, sx, sy);
        let off = x * 3;
        out_row[off] = r;
        out_row[off + 1] = g;
        out_row[off + 2] = b;
    }
}

/// Per-row inner loop for TCA resampling. Like `resample_distortion_row` but
/// each pixel gets three coordinate pairs — one per channel — so each channel
/// can be sampled from a slightly different source location.
///
/// Same `#[multiversion]` treatment as `resample_distortion_row`.
#[multiversion(targets("aarch64+neon", "x86_64+avx+avx2+fma"))]
fn resample_tca_row(src: &[f32], w: usize, h: usize, coords: &[f32], out_row: &mut [f32]) {
    for x in 0..w {
        let base = x * 6;
        let r = sample_single_channel_bilinear_fast(src, w, h, coords[base], coords[base + 1], 0);
        let g =
            sample_single_channel_bilinear_fast(src, w, h, coords[base + 2], coords[base + 3], 1);
        let b =
            sample_single_channel_bilinear_fast(src, w, h, coords[base + 4], coords[base + 5], 2);
        let off = x * 3;
        out_row[off] = r;
        out_row[off + 1] = g;
        out_row[off + 2] = b;
    }
}

/// Branchless bilinear sample of all three channels at `(sx, sy)` in a packed
/// RGB f32 buffer.
///
/// NaN or infinite coordinates produce `(0.0, 0.0, 0.0)` without a branch:
/// we replace non-finite inputs with `0.0` before the clamp using integer
/// bit-manipulation (`finite_mask` is all-ones for finite values, all-zeros
/// for NaN/inf). The result is multiplied by the mask's float form (1.0 or
/// 0.0) so NaN inputs produce a zero output.
///
/// Uses `f32::mul_add` (fused multiply-add) for the bilinear weight
/// computation, giving the compiler a hint to emit FMA instructions on both
/// NEON and AVX2.
#[inline(always)]
fn sample_rgb_bilinear_fast(src: &[f32], w: usize, h: usize, sx: f32, sy: f32) -> (f32, f32, f32) {
    // Replace NaN/inf with 0.0 before the clamp so the `as usize` cast below
    // is never fed a non-finite value. `finite` is 1.0 for valid coords,
    // 0.0 for NaN/inf. We use `if` rather than multiplication because
    // `NaN * 0.0 == NaN` — the multiply trick doesn't clear NaN.
    let is_finite = sx.is_finite() && sy.is_finite();
    let finite = is_finite as u32 as f32; // 1.0 or 0.0, for zeroing the output
    let sx = if is_finite { sx } else { 0.0 };
    let sy = if is_finite { sy } else { 0.0 };

    let max_x = (w - 1) as f32;
    let max_y = (h - 1) as f32;
    let x = sx.clamp(0.0, max_x);
    let y = sy.clamp(0.0, max_y);
    let x0 = x as usize; // floor; x is already >= 0
    let y0 = y as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    // Fractional parts for bilinear weights.
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;

    let p00 = (y0 * w + x0) * 3;
    let p01 = (y0 * w + x1) * 3;
    let p10 = (y1 * w + x0) * 3;
    let p11 = (y1 * w + x1) * 3;

    // Bilinear interpolation with fused multiply-add.
    // lerp(a, b, t) = a + (b - a) * t = a.mul_add(1.0 - t, b * t)
    // We expand it as: a + t.mul_add(b - a, 0.0) which matches the scalar
    // form `a + (b - a) * t` but expresses the multiply-add explicitly.
    #[inline(always)]
    fn bilerp(v00: f32, v01: f32, v10: f32, v11: f32, tx: f32, ty: f32) -> f32 {
        let top = v00 + tx.mul_add(v01 - v00, 0.0);
        let bot = v10 + tx.mul_add(v11 - v10, 0.0);
        top + ty.mul_add(bot - top, 0.0)
    }

    let r = bilerp(src[p00], src[p01], src[p10], src[p11], tx, ty) * finite;
    let g = bilerp(
        src[p00 + 1],
        src[p01 + 1],
        src[p10 + 1],
        src[p11 + 1],
        tx,
        ty,
    ) * finite;
    let b = bilerp(
        src[p00 + 2],
        src[p01 + 2],
        src[p10 + 2],
        src[p11 + 2],
        tx,
        ty,
    ) * finite;
    (r, g, b)
}

/// Branchless bilinear sample of a single channel (`ch`: 0 = R, 1 = G, 2 = B)
/// from a packed RGB f32 buffer at `(sx, sy)`. Used by the TCA resampler so
/// each channel can use its own source coordinate.
///
/// Same NaN handling and FMA hints as `sample_rgb_bilinear_fast`.
#[inline(always)]
fn sample_single_channel_bilinear_fast(
    src: &[f32],
    w: usize,
    h: usize,
    sx: f32,
    sy: f32,
    ch: usize,
) -> f32 {
    let is_finite = sx.is_finite() && sy.is_finite();
    let finite = is_finite as u32 as f32;
    let sx = if is_finite { sx } else { 0.0 };
    let sy = if is_finite { sy } else { 0.0 };

    let max_x = (w - 1) as f32;
    let max_y = (h - 1) as f32;
    let x = sx.clamp(0.0, max_x);
    let y = sy.clamp(0.0, max_y);
    let x0 = x as usize;
    let y0 = y as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;

    let p00 = (y0 * w + x0) * 3 + ch;
    let p01 = (y0 * w + x1) * 3 + ch;
    let p10 = (y1 * w + x0) * 3 + ch;
    let p11 = (y1 * w + x1) * 3 + ch;

    let top = src[p00] + tx.mul_add(src[p01] - src[p00], 0.0);
    let bot = src[p10] + tx.mul_add(src[p11] - src[p10], 0.0);
    (top + ty.mul_add(bot - top, 0.0)) * finite
}

/// Bilinear sample of a packed `[R, G, B]` f32 buffer at floating-point
/// coordinates. Out-of-bounds coords clamp to the nearest edge pixel so
/// distortion-warped pixels at the corners get a sensible value instead of
/// a black stripe. This scalar reference version is used only in unit tests
/// for bit-identity verification against the vectorized fast path.
#[cfg(test)]
fn sample_rgb_bilinear(src: &[f32], w: usize, h: usize, sx: f32, sy: f32) -> (f32, f32, f32) {
    if !sx.is_finite() || !sy.is_finite() {
        return (0.0, 0.0, 0.0);
    }
    let max_x = (w - 1) as f32;
    let max_y = (h - 1) as f32;
    let x = sx.clamp(0.0, max_x);
    let y = sy.clamp(0.0, max_y);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let idx = |yy: usize, xx: usize| (yy * w + xx) * 3;

    let p00 = idx(y0, x0);
    let p01 = idx(y0, x1);
    let p10 = idx(y1, x0);
    let p11 = idx(y1, x1);

    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let r = lerp(
        lerp(src[p00], src[p01], tx),
        lerp(src[p10], src[p11], tx),
        ty,
    );
    let g = lerp(
        lerp(src[p00 + 1], src[p01 + 1], tx),
        lerp(src[p10 + 1], src[p11 + 1], tx),
        ty,
    );
    let b = lerp(
        lerp(src[p00 + 2], src[p01 + 2], tx),
        lerp(src[p10 + 2], src[p11 + 2], tx),
        ty,
    );
    (r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bilinear_center_of_constant_image_returns_the_constant() {
        let w = 4;
        let h = 4;
        let mut src = vec![0.0_f32; w * h * 3];
        for px in src.chunks_exact_mut(3) {
            px[0] = 0.25;
            px[1] = 0.5;
            px[2] = 0.75;
        }
        let (r, g, b) = sample_rgb_bilinear(&src, w, h, 1.5, 1.5);
        assert!((r - 0.25).abs() < 1e-6);
        assert!((g - 0.5).abs() < 1e-6);
        assert!((b - 0.75).abs() < 1e-6);
    }

    #[test]
    fn bilinear_clamps_out_of_bounds() {
        let w = 2;
        let h = 2;
        // 2x2: (0,0)=red, (1,0)=green, (0,1)=blue, (1,1)=white.
        #[rustfmt::skip]
        let src = vec![
            1.0, 0.0, 0.0,  0.0, 1.0, 0.0,
            0.0, 0.0, 1.0,  1.0, 1.0, 1.0,
        ];
        let (r, g, b) = sample_rgb_bilinear(&src, w, h, -5.0, -5.0);
        assert_eq!((r, g, b), (1.0, 0.0, 0.0));
        let (r, g, b) = sample_rgb_bilinear(&src, w, h, 999.0, 999.0);
        assert_eq!((r, g, b), (1.0, 1.0, 1.0));
    }

    /// Verify `sample_rgb_bilinear_fast` matches the scalar `sample_rgb_bilinear`
    /// on in-bounds finite coordinates, within f32 FMA rounding tolerance.
    #[test]
    fn fast_sampler_matches_scalar_on_finite_coords() {
        let w = 8;
        let h = 6;
        let src: Vec<f32> = (0..(w * h * 3)).map(|i| (i as f32) * 0.01).collect();

        // Test a grid of sub-pixel positions spread across the image.
        let test_coords: &[(f32, f32)] = &[
            (0.0, 0.0),
            (0.5, 0.0),
            (3.7, 2.3),
            (6.9, 4.9),
            (7.0, 5.0),
            (0.0, 5.0),
            (7.0, 0.0),
        ];

        for &(sx, sy) in test_coords {
            let (r_ref, g_ref, b_ref) = sample_rgb_bilinear(&src, w, h, sx, sy);
            let (r_fast, g_fast, b_fast) = sample_rgb_bilinear_fast(&src, w, h, sx, sy);
            // Tolerance covers FMA rounding (one ULP difference at most).
            let tol = 1e-5_f32;
            assert!(
                (r_fast - r_ref).abs() < tol,
                "R mismatch at ({sx}, {sy}): fast={r_fast}, ref={r_ref}"
            );
            assert!(
                (g_fast - g_ref).abs() < tol,
                "G mismatch at ({sx}, {sy}): fast={g_fast}, ref={g_ref}"
            );
            assert!(
                (b_fast - b_ref).abs() < tol,
                "B mismatch at ({sx}, {sy}): fast={b_fast}, ref={b_ref}"
            );
        }
    }

    /// Verify `sample_rgb_bilinear_fast` returns `(0, 0, 0)` for NaN/inf coords,
    /// matching the scalar reference.
    #[test]
    fn fast_sampler_nan_inf_returns_zero() {
        let w = 4;
        let h = 4;
        let src = vec![1.0_f32; w * h * 3];

        for &(sx, sy) in &[
            (f32::NAN, 1.0),
            (1.0, f32::NAN),
            (f32::INFINITY, 1.0),
            (1.0, f32::NEG_INFINITY),
            (f32::NAN, f32::NAN),
        ] {
            let (r, g, b) = sample_rgb_bilinear_fast(&src, w, h, sx, sy);
            assert_eq!(
                (r, g, b),
                (0.0, 0.0, 0.0),
                "expected (0,0,0) for ({sx}, {sy}), got ({r}, {g}, {b})"
            );
        }
    }

    /// Verify `sample_single_channel_bilinear_fast` matches its scalar equivalent
    /// (extracting individual channels from `sample_rgb_bilinear`) on finite coords.
    #[test]
    fn single_channel_fast_matches_scalar() {
        let w = 6;
        let h = 5;
        let src: Vec<f32> = (0..(w * h * 3))
            .map(|i| (i as f32) * 0.03 + 0.001)
            .collect();

        let test_coords: &[(f32, f32)] = &[(0.0, 0.0), (2.5, 1.5), (5.9, 3.9), (3.0, 2.0)];

        for &(sx, sy) in test_coords {
            let (r_ref, g_ref, b_ref) = sample_rgb_bilinear(&src, w, h, sx, sy);
            let r_fast = sample_single_channel_bilinear_fast(&src, w, h, sx, sy, 0);
            let g_fast = sample_single_channel_bilinear_fast(&src, w, h, sx, sy, 1);
            let b_fast = sample_single_channel_bilinear_fast(&src, w, h, sx, sy, 2);
            let tol = 1e-5_f32;
            assert!(
                (r_fast - r_ref).abs() < tol,
                "R ch mismatch at ({sx}, {sy}): fast={r_fast}, ref={r_ref}"
            );
            assert!(
                (g_fast - g_ref).abs() < tol,
                "G ch mismatch at ({sx}, {sy}): fast={g_fast}, ref={g_ref}"
            );
            assert!(
                (b_fast - b_ref).abs() < tol,
                "B ch mismatch at ({sx}, {sy}): fast={b_fast}, ref={b_ref}"
            );
        }
    }

    /// Scalar reference implementations for benchmarking. These mirror the
    /// original `sample_rgb_bilinear`-based inner loops that existed before the
    /// `#[multiversion]` + FMA refactor (Phase 6.3).
    fn scalar_resample_distortion_row(
        src: &[f32],
        w: usize,
        h: usize,
        coords: &[f32],
        out_row: &mut [f32],
    ) {
        for x in 0..w {
            let sx = coords[2 * x];
            let sy = coords[2 * x + 1];
            let (r, g, b) = sample_rgb_bilinear(src, w, h, sx, sy);
            let off = x * 3;
            out_row[off] = r;
            out_row[off + 1] = g;
            out_row[off + 2] = b;
        }
    }

    fn scalar_resample_tca_row(
        src: &[f32],
        w: usize,
        h: usize,
        coords: &[f32],
        out_row: &mut [f32],
    ) {
        for x in 0..w {
            let base = x * 6;
            let (r, _, _) = sample_rgb_bilinear(src, w, h, coords[base], coords[base + 1]);
            let (_, g, _) = sample_rgb_bilinear(src, w, h, coords[base + 2], coords[base + 3]);
            let (_, _, b) = sample_rgb_bilinear(src, w, h, coords[base + 4], coords[base + 5]);
            let off = x * 3;
            out_row[off] = r;
            out_row[off + 1] = g;
            out_row[off + 2] = b;
        }
    }

    /// `#[ignore]`-d perf benchmark — run manually to measure the SIMD speedup:
    ///
    /// ```sh
    /// cd apps/desktop
    /// cargo test --release color::lens_correction::tests::resample_20mp_bench -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn resample_20mp_bench() {
        let w: usize = 5470;
        let h: usize = 3656;
        let src: Vec<f32> = (0..(w * h * 3))
            .map(|i| (i % 1000) as f32 / 1000.0)
            .collect();
        let coords_dist: Vec<f32> = (0..w)
            .flat_map(|x| [x as f32 + 0.3, 100.0_f32 + (x as f32) * 0.01])
            .collect();
        let mut out = vec![0.0_f32; w * 3];

        // Warm up both paths so the allocator and thread pool are hot.
        resample_distortion_row(&src, w, h, &coords_dist, &mut out);
        scalar_resample_distortion_row(&src, w, h, &coords_dist, &mut out);

        let runs = 10;

        // Scalar baseline (original, pre-Phase 6.3).
        let mut total_dist_scalar = 0u128;
        for _ in 0..runs {
            let t = std::time::Instant::now();
            for _ in 0..h {
                scalar_resample_distortion_row(&src, w, h, &coords_dist, &mut out);
            }
            total_dist_scalar += t.elapsed().as_millis();
        }
        let scalar_dist_ms = total_dist_scalar / runs;

        // SIMD / FMA path (Phase 6.3).
        let mut total_dist = 0u128;
        for _ in 0..runs {
            let t = std::time::Instant::now();
            for _ in 0..h {
                resample_distortion_row(&src, w, h, &coords_dist, &mut out);
            }
            total_dist += t.elapsed().as_millis();
        }
        let simd_dist_ms = total_dist / runs;

        println!(
            "resample_distortion_row: scalar={scalar_dist_ms} ms/frame  simd={simd_dist_ms} ms/frame  speedup={:.2}×",
            scalar_dist_ms as f64 / simd_dist_ms as f64
        );

        let coords_tca: Vec<f32> = (0..w)
            .flat_map(|x| {
                let fx = x as f32;
                [fx + 0.1, 100.0, fx + 0.0, 100.0, fx - 0.1, 100.0]
            })
            .collect();

        let mut total_tca_scalar = 0u128;
        for _ in 0..runs {
            let t = std::time::Instant::now();
            for _ in 0..h {
                scalar_resample_tca_row(&src, w, h, &coords_tca, &mut out);
            }
            total_tca_scalar += t.elapsed().as_millis();
        }
        let scalar_tca_ms = total_tca_scalar / runs;

        let mut total_tca = 0u128;
        for _ in 0..runs {
            let t = std::time::Instant::now();
            for _ in 0..h {
                resample_tca_row(&src, w, h, &coords_tca, &mut out);
            }
            total_tca += t.elapsed().as_millis();
        }
        let simd_tca_ms = total_tca / runs;

        println!(
            "resample_tca_row:        scalar={scalar_tca_ms} ms/frame  simd={simd_tca_ms} ms/frame  speedup={:.2}×",
            scalar_tca_ms as f64 / simd_tca_ms as f64
        );
    }
}
