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

use std::sync::OnceLock;

use lensfun::{Database, Modifier};
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
            for x in 0..w {
                let sx = coords[2 * x];
                let sy = coords[2 * x + 1];
                let (r, g, b) = sample_rgb_bilinear(&src, w, h, sx, sy);
                let off = x * 3;
                out_row[off] = r;
                out_row[off + 1] = g;
                out_row[off + 2] = b;
            }
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
            for x in 0..w {
                let base = x * 6;
                // Red plane.
                let (r, _, _) = sample_rgb_bilinear(&src, w, h, coords[base], coords[base + 1]);
                // Green plane.
                let (_, g, _) = sample_rgb_bilinear(&src, w, h, coords[base + 2], coords[base + 3]);
                // Blue plane.
                let (_, _, b) = sample_rgb_bilinear(&src, w, h, coords[base + 4], coords[base + 5]);
                let off = x * 3;
                out_row[off] = r;
                out_row[off + 1] = g;
                out_row[off + 2] = b;
            }
        });
}

/// Bilinear sample of a packed `[R, G, B]` f32 buffer at floating-point
/// coordinates. Out-of-bounds coords clamp to the nearest edge pixel so
/// distortion-warped pixels at the corners get a sensible value instead of
/// a black stripe.
#[inline]
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
}
