//! Camera RAW decoding via the `rawler` crate.
//!
//! Covers the 10 formats listed in [`super::dispatch::is_raw_extension`]. The
//! pipeline is a two-phase affair:
//!
//! 1. **Rawler's sensor-stage passes.** `raw_image` pulls the mosaic and
//!    metadata out of the file. We then run rawler's own develop pipeline,
//!    but only up through demosaic + active-area crop, skipping its built-in
//!    calibrate/sRGB-gamma stages. The output is a 3-channel float buffer in
//!    camera RGB, with black/white levels and demosaic already applied.
//! 2. **Our wide-gamut color path.** We pull the camera's D65 color matrix and
//!    white-balance coefficients off the raw metadata, combine them with our
//!    own `XYZ → linear Rec.2020` matrix, and map every pixel through the
//!    resulting `cam → linear Rec.2020` transform. Crucially, we do **not**
//!    clip. Rawler's default pipeline clips to sRGB during `Calibrate`, which
//!    throws away any P3/Rec.2020 coverage the sensor captured. The wide-gamut
//!    intermediate preserves those colors all the way through the final ICC
//!    transform to the display profile.
//! 3. **Baseline exposure lift.** Still in linear Rec.2020 land, we apply a
//!    single EV scale (`linear *= 2^ev`). Source is the DNG
//!    `BaselineExposure` tag (50730) when present, otherwise a +0.5 EV
//!    default that matches what Adobe-neutral viewers apply silently. See
//!    `baseline_exposure_ev` for the priority chain and clamp.
//! 4. **Default tone curve.** A mild filmic S-curve shaped on **luminance
//!    only** in the same linear Rec.2020 working space. Every pixel's RGB
//!    is scaled by the same `Y_out / Y_in`, so hue and chroma are
//!    preserved; only brightness reshapes. Adds midtone contrast with a
//!    soft shoulder at 1.0, closing the "flat look" gap against
//!    Preview.app and Affinity. See `color::tone_curve` for the curve
//!    shape.
//! 5. **Saturation boost.** A mild (+8 %) global chroma scale around the
//!    luminance axis in linear Rec.2020, approximating the "vibrancy" of
//!    Apple's and Affinity's per-camera tuning tables. Preserves hue and
//!    luminance exactly. See `color::saturation`.
//! 6. **Capture sharpening.** After moxcms lands the pixels in display
//!    space and we quantise to RGBA8, a separable-Gaussian unsharp mask
//!    on **luminance only** closes the "crispness gap" against
//!    Preview.app. Y-plane blur in f32, then per-pixel RGB scale by
//!    `Y_out / Y_in`. Avoids the color fringes per-channel sharpening
//!    produces at colored edges. See `color::sharpen` for algorithm and
//!    safety invariants.
//!
//! Moxcms transforms `linear Rec.2020 → display ICC` in f32 land so
//! out-of-[0, 1] values stay meaningful up to the final 8-bit conversion,
//! and sharpening runs on the display-space RGB8 buffer so we match the
//! perceptual response human eyes have on gamma-encoded data.
//!
//! ## Why Rec.2020 instead of Display P3
//!
//! Rec.2020's gamut is wider than Display P3 and fits nearly every
//! photographic color a camera sensor can capture. It's becoming the standard
//! working space for wide-gamut and HDR pipelines. Choosing P3 instead would
//! work, but it could still clip some saturated greens and blues on cameras
//! with wider native gamuts. Rec.2020 costs us nothing extra: moxcms handles
//! it natively. See `docs/notes/raw-support-phase2.md` for the decision trail.
//!
//! ## Orientation quirk
//!
//! `RawImage.orientation` is hard-coded to `Normal` in rawler; the real EXIF
//! orientation lives on `raw_metadata(...).exif.orientation`. We propagate the
//! latter through the shared `apply_orientation` helper in `orientation.rs`.
//!
//! ## Fujifilm X-Trans
//!
//! RAF files use bilinear demosaic only (no Markesteijn). Output is usable but
//! less detailed than a dedicated X-Trans algorithm. Fine for a viewer.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rawler::RawImage;
use rawler::decoders::{Decoder, RawDecodeParams, WellKnownIFD};
use rawler::formats::tiff::Value;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::xyz::Illuminant;
use rawler::rawsource::RawSource;
use rawler::tags::DngTag;
use rayon::prelude::*;

use crate::color;
use crate::color::profiles::REC2020_TO_XYZ_D65;

use super::DecodedImage;

/// Fallback exposure lift when a RAW file doesn't carry a DNG `BaselineExposure`
/// tag. +0.5 EV (~1.41× linear gain) is Adobe's neutral default and roughly
/// matches what Preview.app, Apple Photos, and Lightroom apply silently when
/// opening a sensor-linear file.
const DEFAULT_BASELINE_EV: f32 = 0.5;

/// Safety clamp on the baseline exposure. A well-formed DNG lands in
/// [+0.0, +1.5] EV; anything past ±2 EV is almost certainly a bogus tag and
/// would blow out the whole image.
const BASELINE_EV_CLAMP: f32 = 2.0;

/// Decode a RAW file through our wide-gamut pipeline and color-manage to
/// `target_icc`. Returns the developed RGBA8 buffer plus the EXIF orientation
/// read from rawler's metadata (rawler's own `RawImage.orientation` is always
/// `Normal`, so the caller can't trust that one).
pub(super) fn decode(
    path: &Path,
    bytes: Vec<u8>,
    cancelled: Option<&AtomicBool>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
) -> Result<(DecodedImage, u16), String> {
    check_cancelled(cancelled)?;

    // `new_from_shared_vec` hands ownership over without copying; `new_from_slice`
    // would duplicate the buffer, which hurts on a 40 MB sensor file.
    let src = RawSource::new_from_shared_vec(Arc::new(bytes)).with_path(path);

    check_cancelled(cancelled)?;

    let decoder = rawler::get_decoder(&src)
        .map_err(|e| format!("Couldn't open RAW {}: {e}", path.display()))?;

    check_cancelled(cancelled)?;

    let params = RawDecodeParams::default();
    let raw = decoder
        .raw_image(&src, &params, false)
        .map_err(|e| format!("Couldn't decode RAW {}: {e}", path.display()))?;

    check_cancelled(cancelled)?;

    // Run rawler's sensor-level passes only: rescale, demosaic, and active-area
    // crop. We hand-roll the calibrate + default-crop + gamma stages below so
    // we can keep the intermediate in wide-gamut linear floats.
    let develop = RawDevelop {
        steps: vec![
            ProcessingStep::Rescale,
            ProcessingStep::Demosaic,
            ProcessingStep::CropActiveArea,
        ],
    };
    let intermediate = develop
        .develop_intermediate(&raw)
        .map_err(|e| format!("Couldn't develop RAW {}: {e}", path.display()))?;

    check_cancelled(cancelled)?;

    // Into wide-gamut linear floats. Also apply white-balance + camera matrix
    // in the same pass to save a buffer traversal on big sensor files.
    let (width, height, rec2020) = camera_to_linear_rec2020(&raw, intermediate)
        .ok_or_else(|| format!("Couldn't map camera to Rec.2020 for {}", path.display()))?;

    check_cancelled(cancelled)?;

    // Apply the raw image's default crop, if any. Rawler's own `CropDefault`
    // step does the same thing after calibrate; we just moved it later so the
    // color transform happens on the full active-area buffer.
    let (width, height, mut rec2020) = apply_default_crop(&raw, width, height, rec2020);

    check_cancelled(cancelled)?;

    // Baseline exposure lift. Still in linear Rec.2020, so the math is a
    // single multiplicative scale per component. Applying it here (pre-ICC)
    // keeps relative luminance correct; doing it after gamma encoding would
    // distort midtones.
    let ev = baseline_exposure_ev(decoder.as_ref(), &raw);
    log::debug!(
        "RAW baseline exposure: {:+.2} EV ({:.3}x linear gain) for {}",
        ev,
        2.0_f32.powf(ev),
        path.display()
    );
    apply_exposure(&mut rec2020, ev);

    check_cancelled(cancelled)?;

    // Default tone curve. Mild filmic S-curve shaped on luminance only in the
    // linear Rec.2020 working space, right before the saturation boost and
    // the ICC transform. Each pixel's RGB is scaled uniformly by
    // `Y_out / Y_in` so hue and chroma are preserved — only brightness
    // reshapes. Adds midtone contrast with a soft highlight shoulder so the
    // output stops reading "flat" compared with Preview.app and Affinity.
    // See `color::tone_curve` for the shape and safety invariants.
    log::debug!("RAW applying default tone curve for {}", path.display());
    color::tone_curve::apply_default_tone_curve(&mut rec2020);

    check_cancelled(cancelled)?;

    // Saturation boost. Linear Rec.2020 space, after the tone curve and
    // before the ICC transform. Pushes chroma out from the luminance axis
    // by a small multiplicative factor, approximating the "vibrancy" Apple
    // and Affinity bake into their per-camera tuning tables. Hue and
    // luminance are both preserved; see `color::saturation` for the
    // formula.
    color::saturation::apply_saturation_boost(
        &mut rec2020,
        color::saturation::DEFAULT_SATURATION_BOOST,
    );

    check_cancelled(cancelled)?;

    // Final color conversion: linear Rec.2020 → display ICC, in-place on
    // floats. Staying in f32 through this hop preserves any values that
    // landed outside [0, 1] during the camera-matrix multiply (which can
    // happen for saturated colors outside the display's gamut — the ICC
    // transform gamut-maps them instead of us pre-clipping).
    let source_profile = color::linear_rec2020_profile();
    color::transform_f32_with_profile(
        &mut rec2020,
        &source_profile,
        target_icc,
        use_relative_colorimetric,
    );

    // Down to RGBA8 for the renderer. Clip to [0, 1] here — the display ICC
    // transform has already placed every in-gamut color in the target space.
    let mut rgba = rec2020_to_rgba8(&rec2020);
    drop(rec2020); // free the big float buffer (~12 bytes/pixel) before returning

    check_cancelled(cancelled)?;

    // Capture sharpening. Runs on the display-space RGBA8 buffer, right
    // after the ICC transform and before orientation, so we sharpen in
    // the same perceptual space the user will see the image in. The
    // kernel is small (σ = 0.8 px, 7 taps) and parallelised via rayon.
    // Operates on luminance only: we blur Y, apply the unsharp-mask
    // formula on Y, then scale the original RGB by `Y_out / Y_in` so
    // hue is preserved and no color fringes land at colored edges.
    color::sharpen::sharpen_rgba8_inplace(&mut rgba, width, height);

    check_cancelled(cancelled)?;

    let orientation = decoder
        .raw_metadata(&src, &params)
        .ok()
        .and_then(|meta| meta.exif.orientation)
        .unwrap_or(1);

    Ok((
        DecodedImage {
            width,
            height,
            rgba_data: rgba,
        },
        orientation,
    ))
}

/// Map rawler's camera-RGB intermediate through white balance + the camera's
/// D65 color matrix composed with our XYZ → linear Rec.2020 matrix. The output
/// is a flat `Vec<f32>` in RGB order, length `width * height * 3`.
///
/// Returns `None` if the intermediate is monochrome (viewers for which aren't
/// in scope here) or if the camera exposes no usable color matrix.
fn camera_to_linear_rec2020(
    raw: &RawImage,
    intermediate: Intermediate,
) -> Option<(u32, u32, Vec<f32>)> {
    // Pull a D65 color matrix. If the camera only lists another illuminant,
    // fall back to whatever the first entry is — matches rawler's own
    // preference ordering.
    let matrix = raw
        .color_matrix
        .iter()
        .find(|(ill, _)| **ill == Illuminant::D65)
        .or_else(|| raw.color_matrix.iter().next())
        .map(|(_, m)| m.clone())?;

    // Normalise white-balance coefficients. Rawler encodes NaN when missing.
    let wb = if raw.wb_coeffs[0].is_nan() {
        [1.0, 1.0, 1.0, 1.0]
    } else {
        raw.wb_coeffs
    };

    match intermediate {
        Intermediate::Monochrome(_) => None, // 1-channel sensor — out of scope here
        Intermediate::ThreeColor(pixels) => {
            let xyz_to_cam = flat_matrix_to_3x3(&matrix)?;
            let cam_to_rec2020 = cam_to_rec2020_matrix_3(xyz_to_cam);
            let (w, h) = (pixels.width, pixels.height);
            let out = three_channel_to_rec2020(&pixels.data, &wb, &cam_to_rec2020);
            Some((w as u32, h as u32, out))
        }
        Intermediate::FourColor(pixels) => {
            let xyz_to_cam = flat_matrix_to_4x3(&matrix)?;
            let cam_to_rec2020 = cam_to_rec2020_matrix_4(xyz_to_cam);
            let (w, h) = (pixels.width, pixels.height);
            let out = four_channel_to_rec2020(&pixels.data, &wb, &cam_to_rec2020);
            Some((w as u32, h as u32, out))
        }
    }
}

/// Build the 3-channel `cam → linear Rec.2020` matrix. Same structure as
/// rawler's sRGB calibrate step: normalise rows (so neutral maps to neutral),
/// invert, then apply. The difference is the RGB-primaries matrix: Rec.2020
/// here vs. sRGB there.
fn cam_to_rec2020_matrix_3(xyz_to_cam: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let rgb_to_cam = matrix_multiply_3x3(&xyz_to_cam, &REC2020_TO_XYZ_D65);
    let rgb_to_cam = normalize_rows_3(rgb_to_cam);
    matrix_invert_3x3(rgb_to_cam).unwrap_or(IDENTITY_3)
}

/// 4-channel flavour (for CYGM-style sensors). Uses rawler's own 4x3
/// pseudo-inverse helper via `imgop::matrix`.
fn cam_to_rec2020_matrix_4(xyz_to_cam: [[f32; 3]; 4]) -> [[f32; 4]; 3] {
    use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse};
    // xyz_to_cam: [[f32; 3]; 4], rec2020→xyz: [[f32; 3]; 3] → rgb_to_cam: [[f32; 3]; 4]
    let rgb_to_cam = normalize(multiply(&xyz_to_cam, &REC2020_TO_XYZ_D65));
    pseudo_inverse(rgb_to_cam)
}

fn three_channel_to_rec2020(
    pixels: &[[f32; 3]],
    wb: &[f32; 4],
    cam_to_rec2020: &[[f32; 3]; 3],
) -> Vec<f32> {
    let mut out = vec![0.0_f32; pixels.len() * 3];
    out.par_chunks_exact_mut(3)
        .zip(pixels.par_iter())
        .for_each(|(slot, pix)| {
            // Apply white balance on the fly: the coefficients bring each
            // channel's reference white to a common level before the matrix.
            let r = pix[0] * wb[0];
            let g = pix[1] * wb[1];
            let b = pix[2] * wb[2];
            slot[0] =
                cam_to_rec2020[0][0] * r + cam_to_rec2020[0][1] * g + cam_to_rec2020[0][2] * b;
            slot[1] =
                cam_to_rec2020[1][0] * r + cam_to_rec2020[1][1] * g + cam_to_rec2020[1][2] * b;
            slot[2] =
                cam_to_rec2020[2][0] * r + cam_to_rec2020[2][1] * g + cam_to_rec2020[2][2] * b;
            // No clip: keep wide-gamut values alive for the display ICC.
        });
    out
}

fn four_channel_to_rec2020(
    pixels: &[[f32; 4]],
    wb: &[f32; 4],
    cam_to_rec2020: &[[f32; 4]; 3],
) -> Vec<f32> {
    let mut out = vec![0.0_f32; pixels.len() * 3];
    out.par_chunks_exact_mut(3)
        .zip(pixels.par_iter())
        .for_each(|(slot, pix)| {
            let c = [
                pix[0] * wb[0],
                pix[1] * wb[1],
                pix[2] * wb[2],
                pix[3] * wb[3],
            ];
            slot[0] = cam_to_rec2020[0][0] * c[0]
                + cam_to_rec2020[0][1] * c[1]
                + cam_to_rec2020[0][2] * c[2]
                + cam_to_rec2020[0][3] * c[3];
            slot[1] = cam_to_rec2020[1][0] * c[0]
                + cam_to_rec2020[1][1] * c[1]
                + cam_to_rec2020[1][2] * c[2]
                + cam_to_rec2020[1][3] * c[3];
            slot[2] = cam_to_rec2020[2][0] * c[0]
                + cam_to_rec2020[2][1] * c[1]
                + cam_to_rec2020[2][2] * c[2]
                + cam_to_rec2020[2][3] * c[3];
        });
    out
}

/// Apply `RawImage.crop_area` (or `active_area`) to the developed buffer, if
/// smaller than the buffer itself. Mirrors rawler's `CropDefault` step; we run
/// it after our color transform so the math has access to every sensor pixel.
fn apply_default_crop(
    raw: &RawImage,
    width: u32,
    height: u32,
    pixels: Vec<f32>,
) -> (u32, u32, Vec<f32>) {
    let crop = match raw.crop_area.or(raw.active_area) {
        Some(c) => c,
        None => return (width, height, pixels),
    };

    let active = raw.active_area.unwrap_or(crop);
    let adapted = crop.adapt(&active);
    if adapted.d.w as u32 == width && adapted.d.h as u32 == height {
        return (width, height, pixels);
    }

    let (cw, ch) = (adapted.d.w, adapted.d.h);
    let (cx, cy) = (adapted.p.x, adapted.p.y);
    let stride = width as usize * 3;
    let mut out = vec![0.0_f32; cw * ch * 3];
    for row in 0..ch {
        let src_y = cy + row;
        let src_off = src_y * stride + cx * 3;
        let dst_off = row * cw * 3;
        out[dst_off..dst_off + cw * 3].copy_from_slice(&pixels[src_off..src_off + cw * 3]);
    }
    (cw as u32, ch as u32, out)
}

/// Multiply every linear RGB component by `2^ev`. Called on the wide-gamut
/// buffer pre-ICC, so the math runs in linear light and preserves relative
/// luminance. Doing this post-gamma would distort midtones.
///
/// No clamp here. Out-of-[0, 1] values stay alive until the ICC transform
/// gamut-maps them.
fn apply_exposure(rec2020: &mut [f32], ev: f32) {
    if ev == 0.0 {
        return;
    }
    let gain = 2.0_f32.powf(ev);
    rec2020.par_iter_mut().for_each(|v| *v *= gain);
}

/// Decide the exposure lift (in EV stops) to apply to this RAW frame. Priority:
///
/// 1. `raw.dng_tags` — rawler populates this when building a DNG from a
///    non-DNG raw, so some edge cases land here first.
/// 2. The decoder's root IFD — for parsed DNG files, rawler does not mirror
///    tags into `raw.dng_tags`, so we read `DngTag::BaselineExposure` (50730)
///    straight off the TIFF via [`Decoder::ifd`].
/// 3. Fallback to [`DEFAULT_BASELINE_EV`] (+0.5 EV).
///
/// The result is clamped to `[-BASELINE_EV_CLAMP, +BASELINE_EV_CLAMP]` so a
/// bogus tag can't blow out the whole image.
///
/// There is no per-camera hint for baseline exposure in rawler's data files
/// (hints are format-level quirks, not color-pipeline tuning), so this
/// function intentionally has no camera-hint branch.
fn baseline_exposure_ev(decoder: &dyn Decoder, raw: &RawImage) -> f32 {
    if let Some(value) = raw.dng_tags.get(&(DngTag::BaselineExposure as u16)) {
        return baseline_exposure_ev_from_tag_value(Some(value), DEFAULT_BASELINE_EV);
    }
    if let Ok(Some(ifd)) = decoder.ifd(WellKnownIFD::Root)
        && let Some(entry) = ifd.get_entry(DngTag::BaselineExposure)
    {
        return baseline_exposure_ev_from_tag_value(Some(&entry.value), DEFAULT_BASELINE_EV);
    }
    clamp_ev(DEFAULT_BASELINE_EV)
}

/// Pure helper so we can unit-test the tag-decoding + clamp logic without
/// fabricating a whole `RawImage` or `Decoder`. Returns the clamped EV:
/// decodes an `SRational` / `Rational` / numeric `Value` into `f32`, falls
/// back to `default` on missing or weird shapes, then clamps.
fn baseline_exposure_ev_from_tag_value(value: Option<&Value>, default: f32) -> f32 {
    let raw_ev = value.and_then(tag_value_to_f32).unwrap_or(default);
    clamp_ev(raw_ev)
}

fn clamp_ev(ev: f32) -> f32 {
    if !ev.is_finite() {
        return 0.0;
    }
    ev.clamp(-BASELINE_EV_CLAMP, BASELINE_EV_CLAMP)
}

/// Convert the first element of a TIFF [`Value`] into `f32`. Supports the
/// types the DNG spec allows for `BaselineExposure` (signed rational) plus
/// the usual numeric fallbacks in case a writer used a wider type. Returns
/// `None` on empty vectors or divide-by-zero rationals.
fn tag_value_to_f32(value: &Value) -> Option<f32> {
    match value {
        Value::SRational(v) => v.first().and_then(|r| {
            if r.d == 0 {
                None
            } else {
                Some(r.n as f32 / r.d as f32)
            }
        }),
        Value::Rational(v) => v.first().and_then(|r| {
            if r.d == 0 {
                None
            } else {
                Some(r.n as f32 / r.d as f32)
            }
        }),
        Value::Float(v) => v.first().copied(),
        Value::Double(v) => v.first().map(|x| *x as f32),
        Value::SLong(v) => v.first().map(|x| *x as f32),
        Value::Long(v) => v.first().map(|x| *x as f32),
        Value::SShort(v) => v.first().map(|x| *x as f32),
        Value::Short(v) => v.first().map(|x| *x as f32),
        Value::SByte(v) => v.first().map(|x| *x as f32),
        Value::Byte(v) => v.first().map(|x| *x as f32),
        _ => None,
    }
}

/// Clamp every f32 to [0, 1] and promote to RGBA8 with full alpha. This is the
/// only legitimate clip in the pipeline: it happens post-ICC, in display
/// space, so everything in-gamut is already accurately placed.
fn rec2020_to_rgba8(rec2020: &[f32]) -> Vec<u8> {
    let pixel_count = rec2020.len() / 3;
    let mut rgba = vec![0u8; pixel_count * 4];
    rgba.par_chunks_exact_mut(4)
        .zip(rec2020.par_chunks_exact(3))
        .for_each(|(dst, src)| {
            dst[0] = f32_to_u8(src[0]);
            dst[1] = f32_to_u8(src[1]);
            dst[2] = f32_to_u8(src[2]);
            dst[3] = 255;
        });
    rgba
}

fn f32_to_u8(v: f32) -> u8 {
    let clipped = v.clamp(0.0, 1.0);
    (clipped * 255.0 + 0.5) as u8
}

/// Flatten a row-major 3x3 matrix from rawler's `FlatColorMatrix` (`Vec<f32>`,
/// length 9). Returns `None` for unexpected shapes.
fn flat_matrix_to_3x3(flat: &[f32]) -> Option<[[f32; 3]; 3]> {
    if flat.len() < 9 {
        return None;
    }
    Some([
        [flat[0], flat[1], flat[2]],
        [flat[3], flat[4], flat[5]],
        [flat[6], flat[7], flat[8]],
    ])
}

/// Flatten a row-major 4x3 matrix from rawler's `FlatColorMatrix` (`Vec<f32>`,
/// length 12).
fn flat_matrix_to_4x3(flat: &[f32]) -> Option<[[f32; 3]; 4]> {
    if flat.len() < 12 {
        return None;
    }
    Some([
        [flat[0], flat[1], flat[2]],
        [flat[3], flat[4], flat[5]],
        [flat[6], flat[7], flat[8]],
        [flat[9], flat[10], flat[11]],
    ])
}

const IDENTITY_3: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn matrix_multiply_3x3(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut r = [[0.0_f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for k in 0..3 {
                r[i][j] += a[i][k] * b[k][j];
            }
        }
    }
    r
}

/// Normalise each row so it sums to 1.0. This is the "neutral maps to neutral"
/// step — same trick rawler uses in its own calibrate pass.
fn normalize_rows_3(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut r = [[0.0_f32; 3]; 3];
    for i in 0..3 {
        let sum: f32 = m[i].iter().sum();
        if sum.abs() > f32::EPSILON {
            for j in 0..3 {
                r[i][j] = m[i][j] / sum;
            }
        }
    }
    r
}

/// Cofactor-expansion inverse of a 3x3 matrix. Falls back to `None` if the
/// matrix is singular; the caller uses the identity in that case, which keeps
/// the decode path alive on pathological cameras.
fn matrix_invert_3x3(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < f32::EPSILON {
        return None;
    }
    let inv_det = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ])
}

fn check_cancelled(cancelled: Option<&AtomicBool>) -> Result<(), String> {
    if let Some(flag) = cancelled
        && flag.load(Ordering::Relaxed)
    {
        return Err("cancelled".into());
    }
    Ok(())
}

// Gated to macOS because these tests go through `color::srgb_icc_bytes`, which
// loads the system sRGB profile from `/System/Library/ColorSync/Profiles/` and
// panics on other platforms. The RAW decoder itself is cross-platform.
#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn malformed_bytes_return_error() {
        let bytes = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03];
        let result = decode(
            Path::new("bogus.arw"),
            bytes,
            None,
            color::srgb_icc_bytes(),
            false,
        );
        assert!(result.is_err(), "expected error for malformed bytes");
    }

    #[test]
    fn cancellation_short_circuits() {
        let flag = AtomicBool::new(true);
        let result = decode(
            Path::new("anything.arw"),
            vec![0u8; 32],
            Some(&flag),
            color::srgb_icc_bytes(),
            false,
        );
        assert_eq!(result.err().as_deref(), Some("cancelled"));
    }

    /// Decode the local ARW fixture if it exists. Gated behind `#[ignore]`
    /// because the fixture lives outside the repo. Run with
    /// `cargo test decoding::raw::tests::arw_fixture_decodes -- --ignored`.
    #[test]
    #[ignore]
    fn arw_fixture_bench() {
        let path = Path::new("/tmp/raw/sample1.arw");
        let bytes = std::fs::read(path).expect("fixture missing");
        // Warm up once
        let _ = decode(path, bytes.clone(), None, color::srgb_icc_bytes(), false);
        let mut times = vec![];
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let _ = decode(path, bytes.clone(), None, color::srgb_icc_bytes(), false)
                .expect("decode failed");
            times.push(t.elapsed().as_millis());
        }
        println!("ARW decode times (ms): {times:?}");
    }

    #[test]
    #[ignore]
    fn arw_fixture_decodes() {
        let path = Path::new("/tmp/raw/sample1.arw");
        let bytes = std::fs::read(path).expect("fixture missing");
        let (img, orientation) =
            decode(path, bytes, None, color::srgb_icc_bytes(), false).expect("decode failed");
        assert_eq!(orientation, 1);
        assert_eq!((img.width, img.height), (5456, 3632));
        assert_eq!(img.rgba_data.len(), 5456 * 3632 * 4);
    }

    #[test]
    #[ignore]
    fn dng_fixture_decodes() {
        let path = Path::new("/tmp/raw/sample2.dng");
        let bytes = std::fs::read(path).expect("fixture missing");
        let (img, orientation) =
            decode(path, bytes, None, color::srgb_icc_bytes(), false).expect("decode failed");
        // Pre-orientation dimensions match the POC: 3990x3000 sideways.
        assert_eq!((img.width, img.height), (3990, 3000));
        assert!(matches!(orientation, 6 | 8));
        assert_eq!(img.rgba_data.len(), 3990 * 3000 * 4);
    }

    #[test]
    fn identity_cam_to_rec2020_is_rec2020_to_xyz_inverse() {
        // If the camera matrix is the identity (pretend camera already in XYZ),
        // then cam_to_rec2020 should equal XYZ_TO_REC2020_D65 up to row
        // normalisation. Test the shape: rows sum to ~1, neutral input gives
        // neutral output.
        let m = cam_to_rec2020_matrix_3(IDENTITY_3);
        // Neutral (1, 1, 1) in camera → (1, 1, 1) in Rec.2020 thanks to row
        // normalisation.
        let out = [
            m[0][0] + m[0][1] + m[0][2],
            m[1][0] + m[1][1] + m[1][2],
            m[2][0] + m[2][1] + m[2][2],
        ];
        for (i, v) in out.iter().enumerate() {
            assert!(
                (v - 1.0).abs() < 1e-3,
                "row {i} sum {v} should be ~1 after normalisation"
            );
        }
    }

    #[test]
    fn matrix_invert_is_consistent() {
        let m = [[2.0_f32, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 5.0]];
        let inv = matrix_invert_3x3(m).expect("diagonal matrix is invertible");
        let product = matrix_multiply_3x3(&m, &inv);
        for (i, row) in product.iter().enumerate() {
            for (j, cell) in row.iter().enumerate() {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (cell - want).abs() < 1e-5,
                    "product[{i}][{j}] = {cell}, want {want}"
                );
            }
        }
    }

    #[test]
    fn matrix_invert_singular_returns_none() {
        let m = [[1.0_f32, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        assert!(matrix_invert_3x3(m).is_none());
    }

    #[test]
    fn flat_matrix_parse_rejects_short_input() {
        assert!(flat_matrix_to_3x3(&[1.0, 2.0]).is_none());
        assert!(flat_matrix_to_4x3(&[1.0; 11]).is_none());
    }

    #[test]
    fn apply_exposure_zero_is_noop() {
        let mut buf = vec![0.0_f32, 0.25, 0.5, 0.75, 1.0, 2.0];
        let original = buf.clone();
        apply_exposure(&mut buf, 0.0);
        assert_eq!(buf, original);
    }

    #[test]
    fn apply_exposure_plus_one_doubles() {
        let mut buf = vec![0.1_f32, 0.2, 0.3];
        apply_exposure(&mut buf, 1.0);
        for (got, want) in buf.iter().zip([0.2_f32, 0.4, 0.6]) {
            assert!((got - want).abs() < 1e-5, "got {got}, want {want}");
        }
    }

    #[test]
    fn apply_exposure_minus_one_halves() {
        let mut buf = vec![0.4_f32, 0.8, 1.2];
        apply_exposure(&mut buf, -1.0);
        for (got, want) in buf.iter().zip([0.2_f32, 0.4, 0.6]) {
            assert!((got - want).abs() < 1e-5, "got {got}, want {want}");
        }
    }

    #[test]
    fn apply_exposure_handles_empty_buffer() {
        let mut buf: Vec<f32> = Vec::new();
        apply_exposure(&mut buf, 0.5); // shouldn't panic
        assert!(buf.is_empty());
    }

    #[test]
    fn baseline_ev_missing_tag_returns_default() {
        let ev = baseline_exposure_ev_from_tag_value(None, DEFAULT_BASELINE_EV);
        assert!((ev - 0.5).abs() < 1e-6);
    }

    #[test]
    fn baseline_ev_reads_srational() {
        use rawler::formats::tiff::SRational;
        // +0.75 EV encoded as 3/4
        let value = Value::SRational(vec![SRational::new(3, 4)]);
        let ev = baseline_exposure_ev_from_tag_value(Some(&value), DEFAULT_BASELINE_EV);
        assert!((ev - 0.75).abs() < 1e-6, "got {ev}");
    }

    #[test]
    fn baseline_ev_reads_negative_srational() {
        use rawler::formats::tiff::SRational;
        // -0.5 EV
        let value = Value::SRational(vec![SRational::new(-1, 2)]);
        let ev = baseline_exposure_ev_from_tag_value(Some(&value), DEFAULT_BASELINE_EV);
        assert!((ev + 0.5).abs() < 1e-6, "got {ev}");
    }

    #[test]
    fn baseline_ev_clamps_extreme_values() {
        use rawler::formats::tiff::SRational;
        // +5 EV — way too much, should clamp to +2
        let big = Value::SRational(vec![SRational::new(5, 1)]);
        let ev = baseline_exposure_ev_from_tag_value(Some(&big), DEFAULT_BASELINE_EV);
        assert!((ev - 2.0).abs() < 1e-6, "got {ev}");

        // -10 EV — should clamp to -2
        let small = Value::SRational(vec![SRational::new(-10, 1)]);
        let ev = baseline_exposure_ev_from_tag_value(Some(&small), DEFAULT_BASELINE_EV);
        assert!((ev + 2.0).abs() < 1e-6, "got {ev}");
    }

    #[test]
    fn baseline_ev_divide_by_zero_falls_back() {
        use rawler::formats::tiff::SRational;
        let junk = Value::SRational(vec![SRational::new(1, 0)]);
        let ev = baseline_exposure_ev_from_tag_value(Some(&junk), DEFAULT_BASELINE_EV);
        // Garbage denominator -> fallback to default (+0.5)
        assert!((ev - 0.5).abs() < 1e-6, "got {ev}");
    }

    #[test]
    fn baseline_ev_accepts_float_tag() {
        let value = Value::Float(vec![0.3]);
        let ev = baseline_exposure_ev_from_tag_value(Some(&value), DEFAULT_BASELINE_EV);
        assert!((ev - 0.3).abs() < 1e-6, "got {ev}");
    }

    #[test]
    fn baseline_ev_rejects_wrong_type() {
        use rawler::formats::tiff::TiffAscii;
        // String tag (e.g., if a broken writer used the wrong type) -> fallback
        let value = Value::Ascii(TiffAscii::new("nonsense"));
        let ev = baseline_exposure_ev_from_tag_value(Some(&value), DEFAULT_BASELINE_EV);
        assert!((ev - 0.5).abs() < 1e-6, "got {ev}");
    }

    #[test]
    fn clamp_ev_handles_nan_and_inf() {
        assert_eq!(clamp_ev(f32::NAN), 0.0);
        assert_eq!(clamp_ev(f32::INFINITY), 0.0);
        assert_eq!(clamp_ev(f32::NEG_INFINITY), 0.0);
        assert_eq!(clamp_ev(0.7), 0.7);
    }
}
