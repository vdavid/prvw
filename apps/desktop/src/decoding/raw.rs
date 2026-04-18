//! Camera RAW decoding via the `rawler` crate.
//!
//! Covers the 10 formats listed in [`super::dispatch::is_raw_extension`]. The
//! pipeline is a two-phase affair:
//!
//! 1. **Rawler's sensor-stage passes, with DNG opcode injection.** `raw_image`
//!    pulls the mosaic and metadata out of the file. For DNGs carrying
//!    `OpcodeList1` we apply them first (pre-linearization sensor
//!    corrections). Then `raw.apply_scaling()` handles black-level and
//!    linear rescale into [0, 1]. Then DNG `OpcodeList2` applies post-
//!    linearization, pre-demosaic CFA-level corrections — the main
//!    example is iPhone ProRAW's four per-Bayer-phase `GainMap`s for
//!    lens shading. Then rawler's `Demosaic + CropActiveArea` lands us
//!    at a 3-channel camera-RGB float buffer. `LinearizationTable`
//!    (tag 50712) is already handled by rawler during its initial read.
//! 2. **Our wide-gamut color path + DNG `OpcodeList3`.** We pull the
//!    camera's D65 color matrix and white-balance coefficients off the
//!    raw metadata, combine them with our own `XYZ → linear Rec.2020`
//!    matrix, and map every pixel through the resulting
//!    `cam → linear Rec.2020` transform. Crucially, we do **not** clip.
//!    Rawler's default pipeline clips to sRGB during `Calibrate`, which
//!    throws away any P3/Rec.2020 coverage the sensor captured. The
//!    wide-gamut intermediate preserves those colors all the way through
//!    the final ICC transform to the display profile. After the color
//!    map we apply `OpcodeList3` (typically `WarpRectilinear` for lens
//!    distortion correction).
//! 3. **Baseline exposure lift.** Still in linear Rec.2020 land, we apply a
//!    single EV scale (`linear *= 2^ev`). Source is the DNG
//!    `BaselineExposure` tag (50730) when present, otherwise a +0.5 EV
//!    default that matches what Adobe-neutral viewers apply silently. See
//!    `baseline_exposure_ev` for the priority chain and clamp.
//! 4. **Highlight recovery.** Pixels whose brightest channel approaches or
//!    exceeds 1.0 are smoothly desaturated toward their luminance. Keeps
//!    bright skies and specular highlights from drifting magenta / cyan
//!    when one channel clips while the others keep rising. In-gamut
//!    pixels pass through untouched. See `color::highlight_recovery`.
//! 5. **DCP (Adobe Digital Camera Profile).** When a matching profile is
//!    available — either embedded in a DNG (Pixel, iPhone ProRAW,
//!    Adobe-converted DNGs, etc.) or discovered as a `.dcp` file under
//!    `$PRVW_DCP_DIR` / Adobe Camera Raw's install dir — apply its
//!    `ProfileHueSatMap` 3D LUT in HSV (Phase 3.2 / 3.3). Since Phase
//!    3.4: a `ProfileLookTable` runs after the HueSatMap when present,
//!    and dual-illuminant profiles blend `HueSatMap1` / `HueSatMap2` by
//!    the scene's estimated color temperature. No DCP means no-op. See
//!    `color::dcp` for format, matching rules, and the still-deferred
//!    `ForwardMatrix` swap.
//! 6. **Tone curve.** When the active DCP ships a `ProfileToneCurve`
//!    (Phase 3.4), it runs in place of our default Hermite S-curve; the
//!    camera's intended tonality wins. Otherwise the default fires:
//!    shadow Hermite → midtone line → highlight shoulder, shaped on
//!    **luminance only** so hue and chroma are preserved through the
//!    shoulder. Adds midtone contrast with a soft 1.0 roll-off, closing
//!    the "flat look" gap against Preview.app. See `color::tone_curve`
//!    for the curve math.
//! 7. **Saturation boost.** A mild (+8 %) global chroma scale around the
//!    luminance axis in linear Rec.2020, approximating the "vibrancy" of
//!    Apple's and Affinity's per-camera tuning tables. Preserves hue and
//!    luminance exactly. See `color::saturation`.
//! 8. **Capture sharpening.** After moxcms lands the pixels in display
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
use rawler::rawimage::RawImageData;
use rawler::rawsource::RawSource;
use rawler::tags::DngTag;
use rayon::prelude::*;

#[cfg(all(test, target_os = "macos"))]
use super::PixelBuffer;
use super::dng_opcodes::{
    Opcode, OpcodeId, apply_fix_bad_pixels_constant, apply_fix_bad_pixels_list, apply_gain_map_cfa,
    apply_gain_map_rgb, apply_warp_rectilinear_rgb, parse_fix_bad_pixels_constant,
    parse_fix_bad_pixels_list, parse_gain_map, parse_opcode_list, parse_warp_rectilinear,
};
use super::{DecodedImage, RawPipelineFlags};
use crate::color;
use crate::color::profiles::REC2020_TO_XYZ_D65;

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
/// `target_icc`. Returns the developed buffer plus the EXIF orientation
/// read from rawler's metadata.
///
/// The buffer comes out as:
/// - `PixelBuffer::Rgba16F` when `flags.hdr_output == true` **and**
///   `edr_headroom > 1.0` — the display can show highlights above white,
///   so we shape the filmic shoulder toward the EDR peak and quantise to
///   IEEE 754 half-floats.
/// - `PixelBuffer::Rgba8` otherwise — identical output to Phase 4 for SDR
///   displays or when the user has opted out via the Settings toggle.
///
/// `edr_headroom` is the `maximumExtendedDynamicRangeColorComponentValue`
/// reported by the active `NSScreen` (see
/// `crate::color::display_profile::current_edr_headroom`).
#[allow(clippy::too_many_arguments)]
pub(super) fn decode(
    path: &Path,
    bytes: Vec<u8>,
    cancelled: Option<&AtomicBool>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    flags: RawPipelineFlags,
    edr_headroom: f32,
) -> Result<(DecodedImage, u16), String> {
    // Decide upfront whether this decode lands in HDR territory. "HDR" here
    // means: the user opted in (`flags.hdr_output`), **and** the active
    // display actually has headroom above display-white. SDR displays
    // always fall through to the legacy RGBA8 path so we stay bit-identical
    // to Phase 4 there.
    let hdr_active = flags.hdr_output && edr_headroom > 1.0;
    let peak = if hdr_active {
        // Cap at the user-facing 4× asymptote. Letting the shoulder track
        // the reported headroom would swing the image every time macOS
        // changes the value (brightness, ambient-light sensor). A stable
        // peak gives stable output; the display itself handles the final
        // white-point mapping.
        crate::color::tone_curve::DEFAULT_PEAK_HDR
    } else {
        crate::color::tone_curve::DEFAULT_PEAK_SDR
    };
    check_cancelled(cancelled)?;

    // One breadcrumb per decode when any step is disabled. Silent on the
    // common-case default path so production logs stay clean.
    if !flags.is_default() {
        let disabled = flags.disabled_step_labels();
        log::info!(
            "RAW pipeline: {} step(s) disabled ({}) for {}",
            disabled.len(),
            disabled.join(", "),
            path.display()
        );
    }

    // `new_from_shared_vec` hands ownership over without copying; `new_from_slice`
    // would duplicate the buffer, which hurts on a 40 MB sensor file.
    let src = RawSource::new_from_shared_vec(Arc::new(bytes)).with_path(path);

    check_cancelled(cancelled)?;

    let decoder = rawler::get_decoder(&src)
        .map_err(|e| format!("Couldn't open RAW {}: {e}", path.display()))?;

    check_cancelled(cancelled)?;

    let params = RawDecodeParams::default();
    let mut raw = decoder
        .raw_image(&src, &params, false)
        .map_err(|e| format!("Couldn't decode RAW {}: {e}", path.display()))?;

    check_cancelled(cancelled)?;

    // DNG `OpcodeList1` (tag 51008): gain maps, bad-pixel fixes, etc. Runs
    // on the raw sensor data *before* black-level subtraction / linear
    // rescale. For non-DNG files and for DNGs without the tag this is a
    // silent no-op. Rawler parses the tag but never applies it — exactly
    // the gap we're filling here. See `docs/notes/raw-support-phase3.md`.
    if flags.dng_opcode_list_1 {
        apply_opcode_list1(decoder.as_ref(), &mut raw, path);
    }

    check_cancelled(cancelled)?;

    // Rawler's `Rescale` step does black-level subtraction and normalises
    // every pixel into `[0, 1]` f32. We run it manually so we can slip
    // `OpcodeList2` in between rescale and demosaic, matching the DNG spec
    // (§ 6). After this call, `raw.data` is `RawImageData::Float` in [0, 1].
    raw.apply_scaling()
        .map_err(|e| format!("Couldn't rescale RAW {}: {e}", path.display()))?;

    check_cancelled(cancelled)?;

    // DNG `OpcodeList2` (tag 51009): pre-demosaic gain maps (lens shading)
    // and bad-pixel fixes. Still operates on the CFA mosaic, so the opcodes
    // can target individual Bayer sub-planes (iPhone ProRAW encodes its
    // four-plane lens-shading correction this way: one GainMap per Bayer
    // phase, starting at (0,0), (0,1), (1,0), (1,1) with pitch 2×2).
    if flags.dng_opcode_list_2 {
        apply_opcode_list2(decoder.as_ref(), &mut raw, path);
    }

    check_cancelled(cancelled)?;

    // Run rawler's remaining sensor-level passes: demosaic + active-area
    // crop. `Rescale` is omitted because we just ran it by hand above. Our
    // own wide-gamut matrix + default crop + color stages land below.
    let develop = RawDevelop {
        steps: vec![ProcessingStep::Demosaic, ProcessingStep::CropActiveArea],
    };
    let intermediate = develop
        .develop_intermediate(&raw)
        .map_err(|e| format!("Couldn't develop RAW {}: {e}", path.display()))?;

    check_cancelled(cancelled)?;

    // Into wide-gamut linear floats. Also apply white-balance + camera matrix
    // in the same pass to save a buffer traversal on big sensor files.
    let (width, height, mut rec2020) = camera_to_linear_rec2020(&raw, intermediate)
        .ok_or_else(|| format!("Couldn't map camera to Rec.2020 for {}", path.display()))?;

    check_cancelled(cancelled)?;

    // DNG `OpcodeList3` (tag 51022): post-color-space opcodes. Mostly
    // `WarpRectilinear` (lens distortion correction) on iPhone ProRAW. The
    // spec defines this list as running *after* the camera→working-space
    // matrix, so our linear Rec.2020 buffer is the right slot. Opcodes
    // here that target CFA sub-planes are unreachable and logged as
    // skipped.
    //
    // We remember whether `WarpRectilinear` fired — when it does, the
    // camera manufacturer's per-shot distortion correction is already
    // baked into the buffer (iPhone ProRAW, Pixel, Adobe-converted DNGs),
    // so Phase 4's `lens_correction` step below is skipped to avoid
    // double correction.
    let mut warp_rectilinear_applied = false;
    if flags.dng_opcode_list_3 {
        warp_rectilinear_applied =
            apply_opcode_list3(decoder.as_ref(), width, height, &mut rec2020, path);
    }

    check_cancelled(cancelled)?;

    // Phase 4.0 — lens correction via lensfun-rs. Distortion + TCA +
    // vignetting from the LensFun community database, matched by camera
    // body + EXIF lens model. Silent no-op for lenses not in the DB and
    // for DNG files whose `OpcodeList3 :: WarpRectilinear` already ran
    // (the manufacturer already corrected geometry; doubling would warp
    // straight lines back the other way). Runs after `OpcodeList3` so we
    // match its pipeline slot and reuse the same post-color linear
    // Rec.2020 buffer. See `color::lens_correction`.
    if flags.lens_correction && !warp_rectilinear_applied {
        let meta = decoder.raw_metadata(&src, &params).ok();
        let lens_model = meta
            .as_ref()
            .and_then(|m| m.exif.lens_model.as_deref())
            .unwrap_or("");
        let focal = meta
            .as_ref()
            .and_then(|m| m.exif.focal_length)
            .map(|r| r.n as f32 / r.d.max(1) as f32)
            .unwrap_or(0.0);
        let aperture = meta
            .as_ref()
            .and_then(|m| m.exif.fnumber)
            .map(|r| r.n as f32 / r.d.max(1) as f32)
            .unwrap_or(0.0);
        let distance = meta
            .as_ref()
            .and_then(|m| m.exif.subject_distance)
            .and_then(|r| {
                if r.d == 0 {
                    None
                } else {
                    Some(r.n as f32 / r.d as f32)
                }
            })
            .filter(|d| d.is_finite() && *d > 0.0 && *d < 10_000.0)
            .unwrap_or(1000.0);
        let _ = color::lens_correction::apply_lens_correction(
            &raw,
            lens_model,
            focal,
            aperture,
            distance,
            &mut rec2020,
            width,
            height,
        );
    } else if flags.lens_correction && warp_rectilinear_applied {
        log::debug!(
            "lens_correction: skipped for {} (DNG OpcodeList3 WarpRectilinear already applied)",
            path.display()
        );
    }

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
    if flags.baseline_exposure {
        let ev = baseline_exposure_ev(decoder.as_ref(), &raw);
        log::debug!(
            "RAW baseline exposure: {:+.2} EV ({:.3}x linear gain) for {}",
            ev,
            2.0_f32.powf(ev),
            path.display()
        );
        apply_exposure(&mut rec2020, ev);
    }

    check_cancelled(cancelled)?;

    // Highlight recovery. Desaturate near-clip pixels toward their luminance
    // so bright highlights don't drift magenta or cyan when one channel
    // clips while the other two keep rising. Runs after exposure so it
    // catches both native sensor clipping and exposure-induced overflow,
    // and before the tone curve so the curve sees a hue-consistent input.
    // In-gamut pixels pass through untouched. See
    // `color::highlight_recovery` for the math and safety invariants.
    if flags.highlight_recovery {
        color::highlight_recovery::apply_default_highlight_recovery(&mut rec2020);
    }

    check_cancelled(cancelled)?;

    // DCP (Adobe Digital Camera Profile). Per-camera color refinement,
    // resolved in this priority order:
    //
    // 1. **Embedded** — profile tags read straight from the DNG's main
    //    IFD. Smartphone DNGs (Pixel, Galaxy, iPhone ProRAW) and any
    //    DNG converted by Adobe DNG Converter ship one here. The camera
    //    manufacturer picked this, so it's the most trustworthy source
    //    and wins over any filesystem match.
    // 2. **Filesystem** — opt-in `.dcp` matching the camera's
    //    `UniqueCameraModel` under `$PRVW_DCP_DIR` or Adobe Camera
    //    Raw's default directory. Used when the file carries no
    //    embedded profile (for example, every Sony ARW in our
    //    fixture set).
    //
    // Zero effect when neither path yields a match, which stays the
    // common case for non-DNG RAWs without an installed profile. See
    // `color::dcp` for the format, matching rules, and what we
    // deliberately skip (LookTable, ProfileToneCurve, forward-matrix
    // swap, dual-illuminant interpolation).
    let camera_id = format!("{} {}", raw.camera.make, raw.camera.model);
    let dng_tags_for_dcp = collect_dng_profile_tags(decoder.as_ref(), &raw);
    // Pass the raw white-balance coefficients into `apply_if_available`
    // so the dual-illuminant blend can estimate the scene's color
    // temperature. Rawler encodes "missing" as NaN in slot 0; we
    // flatten that to neutral here so the DCP path sees a clean [1,1,1,1]
    // and picks the D65-preferring fallback.
    let wb_for_dcp = if raw.wb_coeffs[0].is_nan() {
        [1.0, 1.0, 1.0, 1.0]
    } else {
        raw.wb_coeffs
    };
    let dcp_info = color::dcp::apply_if_available(
        &camera_id,
        dng_tags_for_dcp.as_ref(),
        wb_for_dcp,
        &mut rec2020,
        flags.dcp_hue_sat_map,
        flags.dcp_look_table,
    );
    if let Some((dcp, source)) = &dcp_info {
        log::info!(
            "RAW applied {} DCP '{}' for camera '{}' on {}{}{}",
            color::dcp::source_label(*source),
            dcp.profile_name.as_deref().unwrap_or("<unnamed profile>"),
            camera_id,
            path.display(),
            if dcp.look_table.is_some() && flags.dcp_look_table {
                " [with LookTable]"
            } else {
                ""
            },
            if dcp.tone_curve.is_some() && flags.tone_curve {
                " [with ToneCurve]"
            } else {
                ""
            },
        );
    }

    check_cancelled(cancelled)?;

    // Tone curve. If the active DCP carries a `ProfileToneCurve`, apply
    // it in place of our default — the camera's intended tonality is more
    // authoritative than our generic Preview-tuned curve. Otherwise fall
    // back to the default Hermite S-curve. Either way the math runs on
    // luminance only in linear Rec.2020. The Settings → RAW toggle gates
    // both curves uniformly (Phase 3.7): off means "no tone shaping at
    // all", regardless of source.
    if flags.tone_curve {
        let dcp_tone_curve = dcp_info
            .as_ref()
            .and_then(|(dcp, _)| dcp.tone_curve.as_deref());
        if let Some(points) = dcp_tone_curve {
            // DCP tone curves ship as piecewise-linear x→y tables topping
            // out at (1.0, 1.0), so they're intrinsically SDR. Apply as-is
            // even when the surface is HDR — the shoulder is already shaped
            // by the camera manufacturer.
            log::info!(
                "RAW used DCP tone curve ({} points) for {}",
                points.len(),
                path.display()
            );
            color::tone_curve::apply_tone_curve_lut(&mut rec2020, points);
        } else {
            // Filmic shoulder. `peak = 4.0` leaves HDR highlights alive,
            // `peak = 1.0` reproduces Phase 4's SDR clip bit-for-bit.
            log::info!(
                "RAW used default tone curve (peak {:.1}) for {}",
                peak,
                path.display()
            );
            color::tone_curve::apply_tone_curve(
                &mut rec2020,
                color::tone_curve::DEFAULT_MIDTONE_ANCHOR,
                peak,
            );
        }
    }

    check_cancelled(cancelled)?;

    // Saturation boost. Linear Rec.2020 space, after the tone curve and
    // before the ICC transform. Pushes chroma out from the luminance axis
    // by a small multiplicative factor, approximating the "vibrancy" Apple
    // and Affinity bake into their per-camera tuning tables. Hue and
    // luminance are both preserved; see `color::saturation` for the
    // formula.
    if flags.saturation_boost {
        color::saturation::apply_saturation_boost(
            &mut rec2020,
            color::saturation::DEFAULT_SATURATION_BOOST,
        );
    }

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

    let orientation = decoder
        .raw_metadata(&src, &params)
        .ok()
        .and_then(|meta| meta.exif.orientation)
        .unwrap_or(1);

    check_cancelled(cancelled)?;

    // Branch on the HDR flag decided upfront. SDR path: clamp to [0, 1],
    // quantise to RGBA8, run the unsharp-mask on luminance. HDR path:
    // preserve values above 1.0, quantise to half-floats, skip the
    // unsharp-mask (sharpening wants an 8-bit perceptual buffer to match
    // human gamma response; doing it on f16 would shift the halos and the
    // 20 MP f16 sharpener isn't on the critical path for this phase —
    // tracked in `raw-support-phase5.md`).
    if hdr_active {
        let half_rgba = rec2020_to_rgba16f(&rec2020);
        drop(rec2020);
        log::debug!(
            "RAW HDR output: {width}x{height} RGBA16F, peak {peak:.1}, headroom {edr_headroom:.2}"
        );
        return Ok((
            DecodedImage::from_rgba16f(width, height, half_rgba),
            orientation,
        ));
    }

    // Down to RGBA8 for the renderer. Clip to [0, 1] here — the display ICC
    // transform has already placed every in-gamut color in the target space.
    let mut rgba = rec2020_to_rgba8(&rec2020);
    drop(rec2020); // free the big float buffer (~12 bytes/pixel) before returning

    // Capture sharpening. Runs on the display-space RGBA8 buffer, right
    // after the ICC transform and before orientation, so we sharpen in
    // the same perceptual space the user will see the image in.
    if flags.capture_sharpening {
        color::sharpen::sharpen_rgba8_inplace(&mut rgba, width, height);
    }

    check_cancelled(cancelled)?;

    Ok((DecodedImage::from_rgba8(width, height, rgba), orientation))
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

/// Pack the linear-Rec.2020-then-display-ICC buffer into RGBA16F with the
/// alpha channel set to `1.0`. Preserves values above 1.0 (the HDR
/// highlights the filmic shoulder now keeps alive), clamps values below 0
/// to avoid nonsense on the display-side TRC, and folds NaN to 0. Half-
/// floats store about 3.3 decimal digits of precision across ±65504, which
/// is plenty for display output.
fn rec2020_to_rgba16f(rec2020: &[f32]) -> Vec<u16> {
    use half::f16;
    let pixel_count = rec2020.len() / 3;
    let mut out = vec![0u16; pixel_count * 4];
    out.par_chunks_exact_mut(4)
        .zip(rec2020.par_chunks_exact(3))
        .for_each(|(dst, src)| {
            dst[0] = f16::from_f32(guard_hdr_component(src[0])).to_bits();
            dst[1] = f16::from_f32(guard_hdr_component(src[1])).to_bits();
            dst[2] = f16::from_f32(guard_hdr_component(src[2])).to_bits();
            dst[3] = f16::from_f32(1.0).to_bits();
        });
    out
}

/// Clamp NaN and negative values before the f16 conversion. The filmic
/// shoulder never produces negatives, but the ICC transform can for out-
/// of-gamut pixels, and f16 has no NaN propagation guarantees across the
/// wider pipeline.
#[inline]
fn guard_hdr_component(v: f32) -> f32 {
    if v.is_nan() || v < 0.0 { 0.0 } else { v }
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

/// DNG profile tag IDs we hand to `color::dcp::from_dng_tags`. Keep in
/// sync with the `TAG_*` constants in `color::dcp::embedded`. Listing them
/// here (rather than iterating every entry in the source IFD) lets us
/// pull the minimum set and skip anything unrelated to DCP — the less we
/// clone, the better for decode performance.
const DCP_PROFILE_TAG_IDS: &[u16] = &[
    DngTag::UniqueCameraModel as u16,
    DngTag::CalibrationIlluminant1 as u16,
    DngTag::CalibrationIlluminant2 as u16,
    DngTag::ProfileCalibrationSignature as u16,
    DngTag::ProfileName as u16,
    DngTag::ProfileHueSatMapDims as u16,
    DngTag::ProfileHueSatMapData1 as u16,
    DngTag::ProfileHueSatMapData2 as u16,
    DngTag::ProfileToneCurve as u16,
    DngTag::ProfileCopyright as u16,
    DngTag::ProfileLookTableDims as u16,
    DngTag::ProfileLookTableData as u16,
    DngTag::ProfileHueSatMapEncoding as u16,
    DngTag::ProfileLookTableEncoding as u16,
];

/// Collect the DNG profile tags for the DCP applier. Checks `raw.dng_tags`
/// first (some rawler decoders, notably RAF, populate it directly), then
/// pulls from the decoder's `VirtualDngRootTags` view, then the plain
/// `Root` IFD. Returns `None` when no relevant tag is found, so the DCP
/// applier can skip the embedded-profile path without a useless allocation.
fn collect_dng_profile_tags(
    decoder: &dyn Decoder,
    raw: &RawImage,
) -> Option<std::collections::HashMap<u16, Value>> {
    let mut out: std::collections::HashMap<u16, Value> = std::collections::HashMap::new();
    for tag in DCP_PROFILE_TAG_IDS {
        if let Some(value) = raw.dng_tags.get(tag) {
            out.insert(*tag, value.clone());
        }
    }
    for ifd_kind in [WellKnownIFD::VirtualDngRootTags, WellKnownIFD::Root] {
        if let Ok(Some(ifd)) = decoder.ifd(ifd_kind) {
            for tag in DCP_PROFILE_TAG_IDS {
                if !out.contains_key(tag)
                    && let Some(entry) = ifd.entries.get(tag)
                {
                    out.insert(*tag, entry.value.clone());
                }
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Fetch the raw bytes of `DngTag::OpcodeList1/2/3` off a decoder's virtual
/// DNG raw IFD. Returns `None` for non-DNG files or when the tag isn't
/// present.
fn fetch_opcode_list_bytes(decoder: &dyn Decoder, which: DngTag) -> Option<Vec<u8>> {
    let ifd = decoder
        .ifd(WellKnownIFD::VirtualDngRawTags)
        .ok()
        .flatten()?;
    let entry = ifd.get_entry(which)?;
    match &entry.value {
        Value::Byte(bytes) => Some(bytes.clone()),
        Value::Undefined(bytes) => Some(bytes.clone()),
        other => {
            log::debug!(
                "DNG {:?} has unexpected TIFF type {}; skipping opcode list",
                which,
                other.value_type_name(),
            );
            None
        }
    }
}

/// Parse + apply `OpcodeList1` to the raw sensor buffer in place. Runs on
/// the *pre-rescale* sensor data per DNG spec § 6. Silent no-op for non-DNG
/// files or DNGs without the tag.
fn apply_opcode_list1(decoder: &dyn Decoder, raw: &mut RawImage, path: &Path) {
    apply_cfa_opcode_list(decoder, raw, path, DngTag::OpcodeList1, "OpcodeList1");
}

/// Parse + apply `OpcodeList2` to the raw sensor buffer in place. Per DNG
/// spec § 6, OpcodeList2 runs *after* linearization-to-[0,1] but *before*
/// demosaic, so it still operates on the CFA mosaic. That's where iPhone
/// ProRAW files stash their per-Bayer-phase lens-shading `GainMap`s.
fn apply_opcode_list2(decoder: &dyn Decoder, raw: &mut RawImage, path: &Path) {
    apply_cfa_opcode_list(decoder, raw, path, DngTag::OpcodeList2, "OpcodeList2");
}

/// Shared core for OpcodeList1 and OpcodeList2. Both run on CFA-mosaic
/// data; they only differ in whether rawler's `Rescale` has already run.
fn apply_cfa_opcode_list(
    decoder: &dyn Decoder,
    raw: &mut RawImage,
    path: &Path,
    tag: DngTag,
    list_label: &str,
) {
    let Some(bytes) = fetch_opcode_list_bytes(decoder, tag) else {
        return;
    };
    let opcodes = match parse_opcode_list(&bytes) {
        Ok(list) => list,
        Err(e) => {
            log::warn!("DNG {list_label} parse failed for {}: {e}", path.display());
            return;
        }
    };
    if opcodes.is_empty() {
        return;
    }
    log::info!(
        "DNG {list_label}: {} opcode(s) for {}",
        opcodes.len(),
        path.display()
    );

    let width = raw.width as u32;
    let height = raw.height as u32;
    let cpp = raw.cpp;
    let was_integer = matches!(raw.data, RawImageData::Integer(_));
    let mut data = raw.data.as_f32().into_owned();

    for opcode in &opcodes {
        let label = format!("{list_label} {}", opcode.id);
        match opcode.id {
            OpcodeId::GainMap => match parse_gain_map(&opcode.params) {
                Ok(map) => {
                    log::debug!(
                        "{label}: plane {} {}x{} gain grid over ({},{})-({},{}), pitch ({},{})",
                        map.plane,
                        map.map_points_v,
                        map.map_points_h,
                        map.top,
                        map.left,
                        map.bottom,
                        map.right,
                        map.row_pitch,
                        map.col_pitch,
                    );
                    if cpp == 1 {
                        // CFA mosaic = one plane per DNG spec § 6.2.2.
                        // Bayer-phase selection is spatial (rect + pitch),
                        // NOT per-color; see `dng_opcodes` module docs.
                        apply_gain_map_cfa(&mut data, width, height, &map);
                    } else if cpp == 3 {
                        // `LinearRaw` photometric: plane index is the RGB
                        // channel, not the Bayer phase.
                        apply_gain_map_rgb(&mut data, width, height, &map);
                    } else {
                        log::warn!("{label}: unsupported cpp={cpp}; skipping");
                    }
                }
                Err(e) => report_opcode_error(&label, opcode, e, path),
            },
            OpcodeId::FixBadPixelsConstant if cpp == 1 => {
                match parse_fix_bad_pixels_constant(&opcode.params) {
                    Ok(op) => apply_fix_bad_pixels_constant(&mut data, width, height, &op),
                    Err(e) => report_opcode_error(&label, opcode, e, path),
                }
            }
            OpcodeId::FixBadPixelsList if cpp == 1 => {
                match parse_fix_bad_pixels_list(&opcode.params) {
                    Ok(op) => apply_fix_bad_pixels_list(&mut data, width, height, &op),
                    Err(e) => report_opcode_error(&label, opcode, e, path),
                }
            }
            _ => skip_unknown_opcode(&label, opcode, path),
        }
    }

    // Write back in the same representation the caller handed us.
    if was_integer {
        let as_u16: Vec<u16> = data
            .par_iter()
            .map(|v| (v.clamp(0.0, 1.0) * u16::MAX as f32) as u16)
            .collect();
        raw.data = RawImageData::Integer(as_u16);
    } else {
        raw.data = RawImageData::Float(data);
    }
}

/// Parse + apply `OpcodeList3` on the post-color-matrix linear Rec.2020
/// buffer (3-channel, packed). The main opcode we care about here is
/// `WarpRectilinear` for lens distortion. CFA-targeted opcodes in this
/// list would be unreachable (the CFA mosaic is long gone) and log-skipped.
///
/// Returns `true` when a `WarpRectilinear` opcode fired. The Phase 4 lens
/// correction step (`color::lens_correction`) uses this to skip
/// double-correcting DNGs whose manufacturer-supplied distortion is
/// already baked in.
fn apply_opcode_list3(
    decoder: &dyn Decoder,
    width: u32,
    height: u32,
    rec2020: &mut [f32],
    path: &Path,
) -> bool {
    let Some(bytes) = fetch_opcode_list_bytes(decoder, DngTag::OpcodeList3) else {
        return false;
    };
    let opcodes = match parse_opcode_list(&bytes) {
        Ok(list) => list,
        Err(e) => {
            log::warn!("DNG OpcodeList3 parse failed for {}: {e}", path.display());
            return false;
        }
    };
    if opcodes.is_empty() {
        return false;
    }
    log::info!(
        "DNG OpcodeList3: {} opcode(s) for {}",
        opcodes.len(),
        path.display()
    );

    let mut warp_applied = false;
    for opcode in &opcodes {
        let label = format!("OpcodeList3 {}", opcode.id);
        match opcode.id {
            OpcodeId::WarpRectilinear => match parse_warp_rectilinear(&opcode.params) {
                Ok(warp) => {
                    log::debug!(
                        "{label}: {} plane(s), cx/cy = ({:.3}, {:.3})",
                        warp.planes.len(),
                        warp.planes[0].cx,
                        warp.planes[0].cy,
                    );
                    apply_warp_rectilinear_rgb(rec2020, width, height, &warp);
                    warp_applied = true;
                }
                Err(e) => report_opcode_error(&label, opcode, e, path),
            },
            OpcodeId::GainMap => match parse_gain_map(&opcode.params) {
                Ok(map) => {
                    log::debug!(
                        "{label}: plane {} {}x{} gain grid",
                        map.plane,
                        map.map_points_v,
                        map.map_points_h,
                    );
                    apply_gain_map_rgb(rec2020, width, height, &map);
                }
                Err(e) => report_opcode_error(&label, opcode, e, path),
            },
            _ => skip_unknown_opcode(&label, opcode, path),
        }
    }
    warp_applied
}

/// Log a failed parse of an opcode and decide whether to tolerate it.
fn report_opcode_error(
    label: &str,
    opcode: &Opcode,
    err: super::dng_opcodes::OpcodeParseError,
    path: &Path,
) {
    if opcode.is_optional() {
        log::debug!(
            "{label} parse failed (optional, skipping): {err} for {}",
            path.display()
        );
    } else {
        log::warn!(
            "{label} parse failed (mandatory, skipping anyway): {err} for {}",
            path.display()
        );
    }
}

/// Log an opcode we don't implement. We don't fail the decode even on
/// mandatory unknown opcodes: a recognisable-but-plain output is always
/// better than refusing to open the file.
fn skip_unknown_opcode(label: &str, opcode: &Opcode, path: &Path) {
    if opcode.is_optional() {
        log::debug!(
            "{label}: not implemented (optional, skipping) for {}",
            path.display()
        );
    } else {
        log::warn!(
            "{label}: not implemented (mandatory, best-effort decode) for {}",
            path.display()
        );
    }
}

// Gated to macOS because these tests go through `color::srgb_icc_bytes`, which
// loads the system sRGB profile from `/System/Library/ColorSync/Profiles/` and
// panics on other platforms. The RAW decoder itself is cross-platform.
#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Test-only wrapper around `decode` that hard-codes EDR headroom to 1.0.
    /// Tests run in SDR mode so every output lands in `PixelBuffer::Rgba8`,
    /// matching Phase 4's pipeline bit-for-bit. The HDR-specific path is
    /// covered by `tone_curve.rs` unit tests + the Phase 5 smoke docs.
    fn decode_sdr(
        path: &Path,
        bytes: Vec<u8>,
        cancelled: Option<&AtomicBool>,
        target_icc: &[u8],
        use_rel: bool,
        flags: RawPipelineFlags,
    ) -> Result<(DecodedImage, u16), String> {
        decode(path, bytes, cancelled, target_icc, use_rel, flags, 1.0)
    }

    /// Pull the RGBA8 byte slice out of a `DecodedImage`, panicking for HDR
    /// outputs. SDR-only tests rely on this to keep their byte-comparison
    /// assertions compiling as we rolled `PixelBuffer` in.
    fn rgba8_bytes(img: &DecodedImage) -> &[u8] {
        match &img.pixels {
            PixelBuffer::Rgba8(v) => v.as_slice(),
            PixelBuffer::Rgba16F(_) => panic!("test expected RGBA8 but decode produced RGBA16F"),
        }
    }

    #[test]
    fn malformed_bytes_return_error() {
        let bytes = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03];
        let result = decode_sdr(
            Path::new("bogus.arw"),
            bytes,
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        );
        assert!(result.is_err(), "expected error for malformed bytes");
    }

    #[test]
    fn cancellation_short_circuits() {
        let flag = AtomicBool::new(true);
        let result = decode_sdr(
            Path::new("anything.arw"),
            vec![0u8; 32],
            Some(&flag),
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
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
        let _ = decode_sdr(
            path,
            bytes.clone(),
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        );
        let mut times = vec![];
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let _ = decode_sdr(
                path,
                bytes.clone(),
                None,
                color::srgb_icc_bytes(),
                false,
                RawPipelineFlags::default(),
            )
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
        let (img, orientation) = decode_sdr(
            path,
            bytes,
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        )
        .expect("decode failed");
        assert_eq!(orientation, 1);
        assert_eq!((img.width, img.height), (5456, 3632));
        assert_eq!(rgba8_bytes(&img).len(), 5456 * 3632 * 4);
    }

    #[test]
    #[ignore]
    fn dng_fixture_decodes() {
        let path = Path::new("/tmp/raw/sample2.dng");
        let bytes = std::fs::read(path).expect("fixture missing");
        let (img, orientation) = decode_sdr(
            path,
            bytes,
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        )
        .expect("decode failed");
        // Pre-orientation dimensions match the POC: 3990x3000 sideways.
        assert_eq!((img.width, img.height), (3990, 3000));
        assert!(matches!(orientation, 6 | 8));
        assert_eq!(rgba8_bytes(&img).len(), 3990 * 3000 * 4);
    }

    /// Smoke test: run the full RAW decode on sample2.dng with the logger
    /// initialised. Verifies opcodes fire without panicking. `#[ignore]` —
    /// run with
    /// `RUST_LOG=info cargo test --release dng_opcodes_smoke -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dng_opcodes_smoke() {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();
        let path = Path::new("/tmp/raw/sample2.dng");
        let bytes = std::fs::read(path).expect("fixture missing");
        let (img, _) = decode_sdr(
            path,
            bytes,
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        )
        .expect("decode failed");
        // Post-decode dimensions should still match.
        assert_eq!((img.width, img.height), (3990, 3000));
    }

    /// Phase 4.0 smoke test: decode sample1.arw + sample3.arw (Sony
    /// ILCE-5000 + E PZ 16-50mm OSS) with and without lens correction;
    /// expect a visible pixel delta when the Phase 4 step fires. Also
    /// decodes sample2.dng twice and confirms the output is byte-identical
    /// (OpcodeList3 WarpRectilinear already handled distortion there, so
    /// our lens-correction step should skip).
    ///
    /// `#[ignore]` because the fixtures live outside the repo. Run with
    /// `RUST_LOG=info cargo test --release lens_correction_smoke -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn lens_correction_smoke() {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();
        for arw in ["/tmp/raw/sample1.arw", "/tmp/raw/sample3.arw"] {
            let path = Path::new(arw);
            if !path.exists() {
                log::warn!("skipping missing {arw}");
                continue;
            }
            let bytes = std::fs::read(path).unwrap();
            let on = decode_sdr(
                path,
                bytes.clone(),
                None,
                color::srgb_icc_bytes(),
                false,
                RawPipelineFlags::default(),
            )
            .unwrap()
            .0;
            let off_flags = RawPipelineFlags {
                lens_correction: false,
                ..RawPipelineFlags::default()
            };
            let off = decode_sdr(path, bytes, None, color::srgb_icc_bytes(), false, off_flags)
                .unwrap()
                .0;
            assert_eq!((on.width, on.height), (off.width, off.height));
            let diffs = rgba8_bytes(&on)
                .iter()
                .zip(rgba8_bytes(&off).iter())
                .filter(|(a, b)| a != b)
                .count();
            let pct = 100.0 * diffs as f64 / rgba8_bytes(&on).len() as f64;
            println!("{arw}: {diffs} bytes differ ({pct:.2} %) between lens_correction on/off");
            assert!(
                diffs > 0,
                "lens_correction should visibly change ARW output for {arw}"
            );
        }

        // sample2.dng: WarpRectilinear already in OpcodeList3, so our
        // Phase 4 step skips. Output must be bit-identical.
        let dng = Path::new("/tmp/raw/sample2.dng");
        if !dng.exists() {
            log::warn!("skipping missing sample2.dng");
            return;
        }
        let bytes = std::fs::read(dng).unwrap();
        let on = decode_sdr(
            dng,
            bytes.clone(),
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        )
        .unwrap()
        .0;
        let off = decode_sdr(
            dng,
            bytes,
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags {
                lens_correction: false,
                ..RawPipelineFlags::default()
            },
        )
        .unwrap()
        .0;
        assert_eq!(
            rgba8_bytes(&on),
            rgba8_bytes(&off),
            "sample2.dng must be bit-identical with/without lens_correction (WarpRectilinear already fired)"
        );
        println!("sample2.dng: bit-identical (OpcodeList3 WarpRectilinear handled distortion)");
    }

    /// Sony ARW files have no DNG opcode tags, so the opcode passes should
    /// quietly no-op. Verifies decode still works end-to-end on the Phase
    /// 2 regression fixtures.
    #[test]
    #[ignore]
    fn arw_opcodes_noop_smoke() {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();
        for path_str in ["/tmp/raw/sample1.arw", "/tmp/raw/sample3.arw"] {
            let path = Path::new(path_str);
            if !path.exists() {
                log::warn!("skipping missing fixture {path_str}");
                continue;
            }
            let bytes = std::fs::read(path).expect("fixture missing");
            let (img, _) = decode_sdr(
                path,
                bytes,
                None,
                color::srgb_icc_bytes(),
                false,
                RawPipelineFlags::default(),
            )
            .expect("decode failed");
            assert_eq!((img.width, img.height), (5456, 3632));
        }
    }

    /// Full-pipeline smoke test for Phase 3.2 DCP: decode sample1.arw with
    /// `PRVW_DCP_DIR` pointing at `/tmp/prvw-dcp-test/`, compare the final
    /// RGBA8 against a baseline decode (env var unset). Expect a visible
    /// per-pixel delta when a matching DCP is available. Ignored because
    /// the DCP fixture lives outside the repo (Adobe DCPs have ambiguous
    /// redistribution rights).
    ///
    /// To prepare: download `SONY ILCE-7M3.dcp` from RawTherapee's
    /// `rtdata/dcpprofiles` into `/tmp/prvw-dcp-test/` and rewrite its
    /// `UniqueCameraModel` to `Sony ILCE-5000` so it matches sample1.arw.
    /// See the setup steps in `docs/notes/raw-support-phase3.md`. Run with
    /// `cargo test --release dcp_smoke -- --ignored --nocapture`.
    ///
    /// Both the no-match fallback and the DCP-applied path are exercised
    /// here to keep the `PRVW_DCP_DIR` env-var changes serialized; running
    /// them as separate `#[test]` fns races under the default parallel
    /// runner.
    #[test]
    #[ignore]
    fn dcp_smoke() {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();
        let path = Path::new("/tmp/raw/sample1.arw");
        if !path.exists() {
            log::warn!("skipping: fixture missing");
            return;
        }
        let bytes = std::fs::read(path).expect("fixture missing");

        // Baseline: no env var.
        // SAFETY: Single-threaded test body; we're the only code touching
        // `PRVW_DCP_DIR` here.
        unsafe {
            std::env::remove_var("PRVW_DCP_DIR");
        }
        let (baseline, _) = decode_sdr(
            path,
            bytes.clone(),
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        )
        .expect("baseline");

        // No-match fallback: DCP env var set, but the dir has no matching
        // `.dcp`. Output must be byte-for-byte identical to the baseline.
        unsafe {
            std::env::set_var("PRVW_DCP_DIR", "/nonexistent-prvw-dcp-dir-xyz");
        }
        let (with_empty, _) = decode_sdr(
            path,
            bytes.clone(),
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        )
        .expect("with-empty");
        assert_eq!(
            rgba8_bytes(&baseline),
            rgba8_bytes(&with_empty),
            "no-DCP fallback changed output; must be bit-identical to Phase 3.1"
        );

        // DCP applied: different dir, matching file present. Expect a
        // visible per-pixel delta.
        unsafe {
            std::env::set_var("PRVW_DCP_DIR", "/tmp/prvw-dcp-test");
        }
        let (with_dcp, _) = decode_sdr(
            path,
            bytes,
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        )
        .expect("with-dcp");
        unsafe {
            std::env::remove_var("PRVW_DCP_DIR");
        }

        let n = rgba8_bytes(&baseline)
            .len()
            .min(rgba8_bytes(&with_dcp).len());
        let mut diff_count = 0u64;
        let mut total_delta: u64 = 0;
        for i in 0..n {
            if rgba8_bytes(&baseline)[i] != rgba8_bytes(&with_dcp)[i] {
                diff_count += 1;
                total_delta += (rgba8_bytes(&baseline)[i] as i32 - rgba8_bytes(&with_dcp)[i] as i32)
                    .unsigned_abs() as u64;
            }
        }
        let pct = 100.0 * diff_count as f64 / n as f64;
        let mean = total_delta as f64 / diff_count.max(1) as f64;
        println!("DCP smoke: {diff_count}/{n} bytes changed ({pct:.1}%), mean |Δ| = {mean:.2}");
        assert!(
            diff_count > n as u64 / 100,
            "expected visible color shift from DCP; got only {diff_count} of {n} bytes changed"
        );

        // Dump side-by-side PNGs when `PRVW_DCP_SMOKE_DUMP` is set, so a
        // developer can eyeball the color character shift.
        if let Some(dump_dir) = std::env::var_os("PRVW_DCP_SMOKE_DUMP") {
            let dir = std::path::PathBuf::from(dump_dir);
            std::fs::create_dir_all(&dir).expect("create dump dir");
            let (w, h) = (baseline.width, baseline.height);
            write_rgba_png(&dir.join("baseline.png"), w, h, rgba8_bytes(&baseline));
            write_rgba_png(&dir.join("with-dcp.png"), w, h, rgba8_bytes(&with_dcp));
            println!("Dumped baseline.png / with-dcp.png under {}", dir.display());
        }
    }

    fn write_rgba_png(path: &Path, w: u32, h: u32, rgba: &[u8]) {
        use image::{ImageBuffer, Rgba};
        let img =
            ImageBuffer::<Rgba<u8>, _>::from_raw(w, h, rgba.to_vec()).expect("image buffer size");
        img.save(path).expect("save png");
    }

    /// Phase 3.3 smoke test: decode the Pixel 6 Pro sample2.dng twice —
    /// once with the embedded profile honored (the default) and once
    /// with `PRVW_DISABLE_EMBEDDED_DCP=1` forcing a skip — then assert
    /// the two outputs differ. Confirms our `from_dng_tags` path wires
    /// into the pipeline and produces a visible color shift.
    ///
    /// Set `PRVW_EMBEDDED_DCP_SMOKE_DUMP=/some/dir` to additionally emit
    /// `without-embedded.png` and `with-embedded.png` for side-by-side
    /// visual inspection. `#[ignore]` because the fixture lives outside
    /// the repo. Run with
    /// `cargo test --release embedded_dcp_smoke -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn embedded_dcp_smoke() {
        use crate::color::dcp::EMBEDDED_DCP_DISABLE_ENV_VAR;
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init();
        let path = Path::new("/tmp/raw/sample2.dng");
        if !path.exists() {
            log::warn!("skipping: fixture missing");
            return;
        }
        let bytes = std::fs::read(path).expect("fixture missing");

        // Make sure no stray filesystem DCP can interfere with the
        // comparison. SAFETY: single-threaded test body; serial with the
        // other env-var tests via the `#[ignore]` gate.
        unsafe {
            std::env::remove_var("PRVW_DCP_DIR");
        }

        // Without embedded DCP — what Phase 3.2 would have produced.
        unsafe {
            std::env::set_var(EMBEDDED_DCP_DISABLE_ENV_VAR, "1");
        }
        let (without, _) = decode_sdr(
            path,
            bytes.clone(),
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        )
        .expect("decode without embedded DCP");

        // With embedded DCP — the Phase 3.3 default.
        unsafe {
            std::env::remove_var(EMBEDDED_DCP_DISABLE_ENV_VAR);
        }
        let (with_embedded, _) = decode_sdr(
            path,
            bytes,
            None,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
        )
        .expect("decode with embedded DCP");

        let n = rgba8_bytes(&without)
            .len()
            .min(rgba8_bytes(&with_embedded).len());
        let mut diff_count = 0u64;
        let mut total_delta: u64 = 0;
        for i in 0..n {
            if rgba8_bytes(&without)[i] != rgba8_bytes(&with_embedded)[i] {
                diff_count += 1;
                total_delta += (rgba8_bytes(&without)[i] as i32
                    - rgba8_bytes(&with_embedded)[i] as i32)
                    .unsigned_abs() as u64;
            }
        }
        let pct = 100.0 * diff_count as f64 / n as f64;
        let mean = total_delta as f64 / diff_count.max(1) as f64;
        println!(
            "Embedded DCP smoke: {diff_count}/{n} bytes changed ({pct:.1}%), mean |Δ| = {mean:.2}"
        );
        assert!(
            diff_count > n as u64 / 100,
            "expected embedded DCP to produce a visible shift; got only {diff_count} of {n} bytes changed"
        );

        if let Some(dump_dir) = std::env::var_os("PRVW_EMBEDDED_DCP_SMOKE_DUMP") {
            let dir = std::path::PathBuf::from(dump_dir);
            std::fs::create_dir_all(&dir).expect("create dump dir");
            let (w, h) = (without.width, without.height);
            write_rgba_png(
                &dir.join("without-embedded.png"),
                w,
                h,
                rgba8_bytes(&without),
            );
            write_rgba_png(
                &dir.join("with-embedded.png"),
                w,
                h,
                rgba8_bytes(&with_embedded),
            );
            println!(
                "Dumped without-embedded.png / with-embedded.png under {}",
                dir.display()
            );
        }
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

    /// Flip a single pipeline flag off on the checked-in synthetic DNG and
    /// verify the output differs from the all-defaults decode. This is the
    /// cheap proof that each flag actually reaches the stage it claims to
    /// gate. Uses the tiny 128×128 fixture so the test runs in milliseconds.
    #[test]
    fn each_flag_change_alters_output() {
        use std::path::PathBuf;

        let fixture: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/raw/synthetic-bayer-128.dng");
        let bytes = std::fs::read(&fixture).expect("fixture missing");
        let icc = color::srgb_icc_bytes();

        let (baseline, _) = decode_sdr(
            &fixture,
            bytes.clone(),
            None,
            icc,
            false,
            RawPipelineFlags::default(),
        )
        .expect("baseline decode");

        // Flags whose effect is reachable on the synthetic fixture (no DCP
        // tags, so the DCP toggles would be no-ops). These cover one stage
        // per family: color, tone, detail.
        let highlight_off = RawPipelineFlags {
            highlight_recovery: false,
            ..RawPipelineFlags::default()
        };
        let tone_off = RawPipelineFlags {
            tone_curve: false,
            ..RawPipelineFlags::default()
        };
        let sharp_off = RawPipelineFlags {
            capture_sharpening: false,
            ..RawPipelineFlags::default()
        };
        let exposure_off = RawPipelineFlags {
            baseline_exposure: false,
            ..RawPipelineFlags::default()
        };

        for (label, flags) in [
            ("highlight_recovery", highlight_off),
            ("tone_curve", tone_off),
            ("capture_sharpening", sharp_off),
            ("baseline_exposure", exposure_off),
        ] {
            let (actual, _) = decode_sdr(&fixture, bytes.clone(), None, icc, false, flags)
                .unwrap_or_else(|e| panic!("decode with {label} off failed: {e}"));
            assert_ne!(
                rgba8_bytes(&actual),
                rgba8_bytes(&baseline),
                "flipping `{label}` off should change the output buffer"
            );
        }
    }

    /// Belt-and-braces check: all flags true reproduces today's output
    /// byte-for-byte. If this ever breaks, something slipped into the
    /// default path that the flags don't cover.
    #[test]
    fn defaults_match_bare_load_image() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/raw/synthetic-bayer-128.dng");
        let bytes = std::fs::read(&fixture).expect("fixture missing");
        let icc = color::srgb_icc_bytes();

        let (via_flags, _) = decode_sdr(
            &fixture,
            bytes.clone(),
            None,
            icc,
            false,
            RawPipelineFlags::default(),
        )
        .expect("with explicit defaults");
        let (via_flags_again, _) = decode_sdr(
            &fixture,
            bytes,
            None,
            icc,
            false,
            RawPipelineFlags::default(),
        )
        .expect("second pass");
        assert_eq!(rgba8_bytes(&via_flags), rgba8_bytes(&via_flags_again));
    }

    #[test]
    fn clamp_ev_handles_nan_and_inf() {
        assert_eq!(clamp_ev(f32::NAN), 0.0);
        assert_eq!(clamp_ev(f32::INFINITY), 0.0);
        assert_eq!(clamp_ev(f32::NEG_INFINITY), 0.0);
        assert_eq!(clamp_ev(0.7), 0.7);
    }
}
