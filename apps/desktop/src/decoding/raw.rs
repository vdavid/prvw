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
//!
//! Then moxcms transforms `linear Rec.2020 → display ICC` in f32 land so
//! out-of-[0, 1] values stay meaningful up to the final 8-bit conversion.
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
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::xyz::Illuminant;
use rawler::rawsource::RawSource;
use rayon::prelude::*;

use crate::color;
use crate::color::profiles::REC2020_TO_XYZ_D65;

use super::DecodedImage;

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
    let rgba = rec2020_to_rgba8(&rec2020);
    drop(rec2020); // free the big float buffer (~12 bytes/pixel) before returning

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
}
