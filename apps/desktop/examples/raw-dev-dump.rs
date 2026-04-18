//! Per-stage RAW pipeline inspector.
//!
//! Takes a RAW file and dumps labeled PNGs for each meaningful pipeline stage.
//! Current stages:
//!
//! - `after-opcode1` — raw mosaic (u16) after any DNG `OpcodeList1` opcodes
//!   are applied. For non-DNG files this is the plain rescaled mosaic. Same
//!   as `after-opcode2` on ARW/CR2/NEF.
//! - `after-opcode2` — mosaic after `OpcodeList2` (lens-shading gain maps
//!   etc.). This is what rawler's demosaic sees.
//! - `post-demosaic` — rawler's demosaic + active-area crop. Camera-native
//!   RGB, pre-white-balance. Looks green and dark because WB and the camera
//!   matrix haven't landed yet.
//! - `post-wb` — same buffer, with white-balance coefficients applied.
//! - `linear-rec2020` — after our `cam → XYZ → linear Rec.2020` matrix. Still
//!   wide-gamut linear; PNG-encoded with an sRGB-like gamma so you can eyeball
//!   it on a normal display. Values outside sRGB are clipped in the preview
//!   only — the real pipeline keeps them.
//! - `after-opcode3` — linear Rec.2020 after `OpcodeList3` (WarpRectilinear
//!   lens distortion correction). For non-DNG files this matches
//!   `linear-rec2020`.
//! - `before-lens-correction` / `after-lens-correction` — Phase 4.0
//!   linear Rec.2020 before / after the `lensfun-rs` lens correction
//!   (distortion + TCA + vignetting). Skipped for DNGs whose
//!   `OpcodeList3 :: WarpRectilinear` already handled distortion (both
//!   preview PNGs come out byte-identical in that case).
//! - `post-exposure` — linear Rec.2020 after the baseline-exposure lift
//!   (Phase 2.2). Same sRGB-ish preview encoding as `linear-rec2020` so
//!   side-by-side brightness changes are eyeballable.
//! - `post-highlight-recovery` — linear Rec.2020 after the desaturate-to-
//!   luminance highlight recovery (Phase 3.1). Near-clip pixels (bright
//!   skies, specular highlights) drift toward their luminance instead of
//!   shifting hue. Side-by-side with `post-exposure` the change is most
//!   visible in the brightest regions of the frame; in-gamut pixels
//!   pass through identical.
//! - `post-tone` — linear Rec.2020 after the default tone curve (Phase 2.3,
//!   moved to luminance-only in Phase 2.5a). Same sRGB-ish preview encoding;
//!   the mild S-curve's contrast punch and highlight shoulder should be
//!   visible side-by-side with `post-exposure`, with saturation preserved
//!   through the shoulder.
//! - `post-saturation` — linear Rec.2020 after the mild global saturation
//!   boost (Phase 2.5a). Same sRGB-ish preview encoding; chroma should be
//!   slightly higher than `post-tone`, hue unchanged.
//! - `post-icc` — RGBA8 after the ICC transform to sRGB but BEFORE capture
//!   sharpening. Useful for eyeballing the pre-sharpen softness next to
//!   `final`.
//! - `final` — the RGBA8 buffer Prvw actually renders, after ICC transform
//!   AND capture sharpening (luminance-only since Phase 2.5a). Side-by-side
//!   with `post-icc` the mild crispening should be visible at 1:1 zoom
//!   without color fringes at edges.
//!
//! ## Usage
//!
//! ```sh
//! cd apps/desktop
//! cargo run --example raw-dev-dump -- path/to/file.dng
//! cargo run --example raw-dev-dump -- file.arw --out-dir /tmp/my-dump
//! ```
//!
//! Output defaults to `/tmp/prvw-dev-dump-<filename>/`. The example builds the
//! pipeline directly (not through the app's `load_image`) so it can dump
//! intermediate stages; orientation handling and the dispatcher are skipped.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use image::{ImageBuffer, Rgb};
use moxcms::{
    ColorProfile, InPlaceTransformExecutor, Layout, LocalizableString, ProfileText,
    RenderingIntent, ToneReprCurve, TransformOptions,
};
use rawler::RawImage;
use rawler::decoders::{Decoder, RawDecodeParams, WellKnownIFD};
use rawler::formats::tiff::Value;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::RawImageData;
use rawler::rawsource::RawSource;
use rawler::tags::DngTag;

/// Keep in sync with `src/decoding/raw.rs::DEFAULT_BASELINE_EV`.
const DEFAULT_BASELINE_EV: f32 = 0.5;
/// Keep in sync with `src/decoding/raw.rs::BASELINE_EV_CLAMP`.
const BASELINE_EV_CLAMP: f32 = 2.0;

/// Rec.2020 D65 → XYZ matrix, duplicated here so the example doesn't need a
/// lib-crate split of the desktop app. Keep in sync with
/// `src/color/profiles.rs::REC2020_TO_XYZ_D65`.
#[allow(clippy::excessive_precision)]
const REC2020_TO_XYZ_D65: [[f32; 3]; 3] = [
    [0.6369580, 0.1446169, 0.1688810],
    [0.2627002, 0.6779981, 0.0593017],
    [0.0000000, 0.0280727, 1.0609851],
];

const SRGB_PROFILE_PATH: &str = "/System/Library/ColorSync/Profiles/sRGB Profile.icc";

#[derive(Parser, Debug)]
#[command(about = "Dump each RAW pipeline stage to labeled PNGs")]
struct Args {
    /// Path to the RAW file to decode.
    raw: PathBuf,

    /// Output directory. Defaults to /tmp/prvw-dev-dump-<filename>/.
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

/// One pipeline stage. Write its RGB8 output to a labeled PNG.
struct Stage {
    name: &'static str,
    width: u32,
    height: u32,
    rgb: Vec<u8>,
    took_ms: u128,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let raw_path = args.raw.as_path();
    let out_dir = args.out_dir.unwrap_or_else(|| default_out_dir(raw_path));
    std::fs::create_dir_all(&out_dir)?;

    println!("Input  : {}", raw_path.display());
    println!("Output : {}", out_dir.display());

    let stages = run_pipeline(raw_path)?;

    for stage in &stages {
        let out_path = out_dir.join(format!("{}.png", stage.name));
        save_rgb_png(&out_path, stage.width, stage.height, &stage.rgb)?;
        println!(
            "{:20} {}x{}  {} ms  {}",
            stage.name,
            stage.width,
            stage.height,
            stage.took_ms,
            out_path.display()
        );
    }

    Ok(())
}

/// Default output directory: `/tmp/prvw-dev-dump-<filename>/`.
fn default_out_dir(raw_path: &Path) -> PathBuf {
    let stem = raw_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("raw");
    std::env::temp_dir().join(format!("prvw-dev-dump-{stem}"))
}

/// Run each pipeline phase and collect a stage per inspectable output.
fn run_pipeline(path: &Path) -> Result<Vec<Stage>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let src = RawSource::new_from_shared_vec(Arc::new(bytes)).with_path(path);

    let t_total = Instant::now();
    let decoder = rawler::get_decoder(&src)?;
    let params = RawDecodeParams::default();
    let mut raw = decoder.raw_image(&src, &params, false)?;

    // Stage 0a: OpcodeList1 — applied on the unrescaled sensor buffer.
    let t0 = Instant::now();
    apply_opcode_list(
        decoder.as_ref(),
        &mut raw,
        DngTag::OpcodeList1,
        "OpcodeList1",
    );
    let after_opcode1_preview = cfa_preview(&raw);
    let (raw_w, raw_h) = (raw.width as u32, raw.height as u32);
    let after_opcode1_ms = t0.elapsed().as_millis();

    // Rescale to linear [0, 1]. We split this off from rawler's batch so we
    // can sneak OpcodeList2 in between.
    raw.apply_scaling()?;

    // Stage 0b: OpcodeList2 — applied on the rescaled mosaic.
    let t0 = Instant::now();
    apply_opcode_list(
        decoder.as_ref(),
        &mut raw,
        DngTag::OpcodeList2,
        "OpcodeList2",
    );
    let after_opcode2_preview = cfa_preview(&raw);
    let after_opcode2_ms = t0.elapsed().as_millis();

    // Stage 1: rawler's remaining sensor-level develop steps.
    let t0 = Instant::now();
    let develop = RawDevelop {
        steps: vec![ProcessingStep::Demosaic, ProcessingStep::CropActiveArea],
    };
    let intermediate = develop.develop_intermediate(&raw)?;
    let (demosaic_w, demosaic_h, demosaic_rgb_f32) = intermediate_to_rgb_f32(&intermediate);
    let post_demosaic_ms = t0.elapsed().as_millis();

    // Stage 2: apply white balance only (no matrix yet). Handy to sanity-check
    // that the WB coefficients are reasonable.
    let t0 = Instant::now();
    let wb = white_balance(&raw);
    let post_wb_rgb_f32 = apply_wb_only(&demosaic_rgb_f32, &wb);
    let post_wb_ms = t0.elapsed().as_millis();

    // Stage 3: full cam → linear Rec.2020. Encoded for preview with an sRGB
    // gamma so it's eyeballable on a normal display.
    let t0 = Instant::now();
    let mut rec2020 = camera_to_linear_rec2020(&raw, &demosaic_rgb_f32, &wb);
    let linear_preview = linear_to_srgb_preview(&rec2020);
    let linear_rec2020_ms = t0.elapsed().as_millis();

    // Stage 3b: OpcodeList3 (lens distortion / post-color opcodes).
    let t0 = Instant::now();
    let warp_applied =
        apply_opcode_list3_rgb(decoder.as_ref(), demosaic_w, demosaic_h, &mut rec2020);
    let after_opcode3_preview = linear_to_srgb_preview(&rec2020);
    let after_opcode3_ms = t0.elapsed().as_millis();

    // Stage 3c: Phase 4.0 — lens correction via lensfun-rs. Mirrors
    // `src/decoding/raw.rs` — skipped when OpcodeList3 already ran
    // WarpRectilinear (DNG files). Produces a before/after preview so
    // barrel distortion rectification and corner-vignette lift land in
    // the dump side-by-side.
    let before_lens_correction_preview = linear_to_srgb_preview(&rec2020);
    let t0 = Instant::now();
    let lens_fired = if !warp_applied {
        let params = RawDecodeParams::default();
        let meta = decoder.raw_metadata(&src, &params).ok();
        let lens_model = meta
            .as_ref()
            .and_then(|m| m.exif.lens_model.as_deref())
            .unwrap_or("")
            .to_string();
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
        if !lens_model.is_empty() && focal > 0.0 && aperture > 0.0 {
            apply_lens_correction_stage(
                &raw,
                &lens_model,
                focal,
                aperture,
                distance,
                &mut rec2020,
                demosaic_w,
                demosaic_h,
            )
        } else {
            false
        }
    } else {
        println!("  lens_correction: skipped (OpcodeList3 WarpRectilinear already applied)");
        false
    };
    let after_lens_correction_preview = linear_to_srgb_preview(&rec2020);
    let lens_correction_ms = t0.elapsed().as_millis();
    if lens_fired {
        println!("  lens_correction: applied");
    } else if !warp_applied {
        println!("  lens_correction: no-op (no lens match, or no calibration)");
    }

    // Stage 4: baseline exposure lift (Phase 2.2). Same sRGB-ish preview
    // encoding as the linear stage above so brightness differences are
    // eyeballable side-by-side.
    let t0 = Instant::now();
    let ev = baseline_exposure_ev(decoder.as_ref(), &raw);
    println!(
        "Baseline EV     : {:+.2} ({:.3}x linear)",
        ev,
        2.0_f32.powf(ev)
    );
    let mut rec2020_lifted = rec2020.clone();
    apply_exposure(&mut rec2020_lifted, ev);
    let post_exposure_preview = linear_to_srgb_preview(&rec2020_lifted);
    let post_exposure_ms = t0.elapsed().as_millis();

    // Stage 4b: highlight recovery (Phase 3.1). Desaturate near-clip pixels
    // toward their luminance so bright skies and specular highlights
    // don't drift magenta / cyan when one channel clips while the other
    // two keep rising. Same sRGB-ish preview encoding so the hue rescue
    // is eyeballable against `post-exposure`.
    let t0 = Instant::now();
    let mut rec2020_recovered = rec2020_lifted.clone();
    apply_default_highlight_recovery(&mut rec2020_recovered);
    let post_highlight_recovery_preview = linear_to_srgb_preview(&rec2020_recovered);
    let post_highlight_recovery_ms = t0.elapsed().as_millis();

    // Stage 5: default tone curve (Phase 2.3 / 2.5a). Mild filmic S-curve
    // shaped on luminance only; every pixel's RGB is scaled uniformly by
    // `Y_out / Y_in`. Same sRGB-ish preview encoding so the added contrast
    // is eyeballable against `post-highlight-recovery`.
    let t0 = Instant::now();
    let mut rec2020_toned = rec2020_recovered.clone();
    apply_default_tone_curve(&mut rec2020_toned);
    let post_tone_preview = linear_to_srgb_preview(&rec2020_toned);
    let post_tone_ms = t0.elapsed().as_millis();

    // Stage 6: saturation boost (Phase 2.5a). Mild +8 % chroma scale in
    // linear Rec.2020, preserving hue and luminance.
    let t0 = Instant::now();
    let mut rec2020_sat = rec2020_toned.clone();
    apply_saturation_boost(&mut rec2020_sat, SATURATION_BOOST);
    let post_saturation_preview = linear_to_srgb_preview(&rec2020_sat);
    let post_saturation_ms = t0.elapsed().as_millis();

    // Stage 7: ICC-transformed sRGB output, pre-sharpening.
    let t0 = Instant::now();
    let mut rec2020_for_icc = rec2020_sat;
    transform_f32_rec2020_to_srgb(&mut rec2020_for_icc);
    let post_icc_rgb = f32_to_rgb8(&rec2020_for_icc);
    let post_icc_ms = t0.elapsed().as_millis();

    // Stage 8: capture sharpening (Phase 2.5a). Luminance-only unsharp
    // mask on the display-space RGB8 buffer: blur Y, apply the unsharp
    // formula on Y, scale RGB by `Y_out / Y_in`. Output is what Prvw
    // actually renders.
    let t0 = Instant::now();
    let mut final_rgb = post_icc_rgb.clone();
    sharpen_rgb8_inplace(&mut final_rgb, demosaic_w, demosaic_h);
    let final_ms = t0.elapsed().as_millis();

    println!("Total decode    : {} ms", t_total.elapsed().as_millis());

    let stages = vec![
        Stage {
            name: "after-opcode1",
            width: raw_w,
            height: raw_h,
            rgb: after_opcode1_preview,
            took_ms: after_opcode1_ms,
        },
        Stage {
            name: "after-opcode2",
            width: raw_w,
            height: raw_h,
            rgb: after_opcode2_preview,
            took_ms: after_opcode2_ms,
        },
        Stage {
            name: "post-demosaic",
            width: demosaic_w,
            height: demosaic_h,
            rgb: f32_to_rgb8_normalized(&demosaic_rgb_f32),
            took_ms: post_demosaic_ms,
        },
        Stage {
            name: "post-wb",
            width: demosaic_w,
            height: demosaic_h,
            rgb: f32_to_rgb8_normalized(&post_wb_rgb_f32),
            took_ms: post_wb_ms,
        },
        Stage {
            name: "linear-rec2020",
            width: demosaic_w,
            height: demosaic_h,
            rgb: linear_preview,
            took_ms: linear_rec2020_ms,
        },
        Stage {
            name: "after-opcode3",
            width: demosaic_w,
            height: demosaic_h,
            rgb: after_opcode3_preview,
            took_ms: after_opcode3_ms,
        },
        Stage {
            name: "before-lens-correction",
            width: demosaic_w,
            height: demosaic_h,
            rgb: before_lens_correction_preview,
            took_ms: 0,
        },
        Stage {
            name: "after-lens-correction",
            width: demosaic_w,
            height: demosaic_h,
            rgb: after_lens_correction_preview,
            took_ms: lens_correction_ms,
        },
        Stage {
            name: "post-exposure",
            width: demosaic_w,
            height: demosaic_h,
            rgb: post_exposure_preview,
            took_ms: post_exposure_ms,
        },
        Stage {
            name: "post-highlight-recovery",
            width: demosaic_w,
            height: demosaic_h,
            rgb: post_highlight_recovery_preview,
            took_ms: post_highlight_recovery_ms,
        },
        Stage {
            name: "post-tone",
            width: demosaic_w,
            height: demosaic_h,
            rgb: post_tone_preview,
            took_ms: post_tone_ms,
        },
        Stage {
            name: "post-saturation",
            width: demosaic_w,
            height: demosaic_h,
            rgb: post_saturation_preview,
            took_ms: post_saturation_ms,
        },
        Stage {
            name: "post-icc",
            width: demosaic_w,
            height: demosaic_h,
            rgb: post_icc_rgb,
            took_ms: post_icc_ms,
        },
        Stage {
            name: "final",
            width: demosaic_w,
            height: demosaic_h,
            rgb: final_rgb,
            took_ms: final_ms,
        },
    ];

    Ok(stages)
}

/// Same exposure math as `src/decoding/raw.rs::apply_exposure`, inlined so
/// the example stays standalone.
fn apply_exposure(rec2020: &mut [f32], ev: f32) {
    if ev == 0.0 {
        return;
    }
    let gain = 2.0_f32.powf(ev);
    for v in rec2020.iter_mut() {
        *v *= gain;
    }
}

/// Default tone curve — mirrors `src/color/tone_curve.rs`. Inlined so the
/// example stays a standalone binary. Keep the constants, shape, and
/// luminance-only apply pattern in sync.
const TONE_SHADOW_KNEE: f32 = 0.10;
const TONE_HIGHLIGHT_KNEE: f32 = 0.90;
const TONE_MIDTONE_SLOPE: f32 = 1.08;
const TONE_MIDTONE_ANCHOR: f32 = 0.40;
const TONE_SHADOW_ENDPOINT_SLOPE: f32 = 1.0;
const TONE_HIGHLIGHT_ENDPOINT_SLOPE: f32 = 0.30;

/// Rec.2020 luma coefficients — tone curve runs in linear Rec.2020.
const TONE_LUMA_R: f32 = 0.2627;
const TONE_LUMA_G: f32 = 0.6780;
const TONE_LUMA_B: f32 = 0.0593;

/// Keep in sync with `src/color/tone_curve.rs::DARK_EPSILON`.
const TONE_DARK_EPSILON: f32 = 1.0e-5;

/// Keep in sync with `src/color/saturation.rs::DEFAULT_SATURATION_BOOST`.
const SATURATION_BOOST: f32 = 0.08;

fn apply_default_tone_curve(rgb: &mut [f32]) {
    for pixel in rgb.chunks_exact_mut(3) {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let y_in = TONE_LUMA_R * r + TONE_LUMA_G * g + TONE_LUMA_B * b;
        if !y_in.is_finite() || y_in < TONE_DARK_EPSILON {
            pixel[0] = 0.0;
            pixel[1] = 0.0;
            pixel[2] = 0.0;
            continue;
        }
        let y_out = default_tone_curve(y_in);
        let scale = y_out / y_in;
        pixel[0] = r * scale;
        pixel[1] = g * scale;
        pixel[2] = b * scale;
    }
}

/// Highlight recovery — mirrors `src/color/highlight_recovery.rs`.
/// Desaturates near-clip pixels toward their luminance via a smoothstep
/// ramp between `threshold` and `ceiling`. Inlined so the example stays
/// standalone; keep constants and formula in sync with the real module.
const HIGHLIGHT_RECOVERY_THRESHOLD: f32 = 0.95;
const HIGHLIGHT_RECOVERY_CEILING: f32 = 1.20;

fn apply_default_highlight_recovery(rgb: &mut [f32]) {
    let (threshold, ceiling) = (HIGHLIGHT_RECOVERY_THRESHOLD, HIGHLIGHT_RECOVERY_CEILING);
    let denom = ceiling - threshold;
    for pixel in rgb.chunks_exact_mut(3) {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let m = r.max(g).max(b);
        if !m.is_finite() || m <= threshold {
            continue;
        }
        let t = if denom <= 0.0 {
            1.0
        } else {
            let s = ((m - threshold) / denom).clamp(0.0, 1.0);
            s * s * (3.0 - 2.0 * s)
        };
        let y = TONE_LUMA_R * r + TONE_LUMA_G * g + TONE_LUMA_B * b;
        pixel[0] = r + (y - r) * t;
        pixel[1] = g + (y - g) * t;
        pixel[2] = b + (y - b) * t;
    }
}

/// Saturation boost — mirrors `src/color/saturation.rs`. Scales each
/// channel's delta-from-luma by `1 + boost`, preserving hue and luminance.
fn apply_saturation_boost(rgb: &mut [f32], boost: f32) {
    if boost == 0.0 {
        return;
    }
    let scale = 1.0 + boost;
    for pixel in rgb.chunks_exact_mut(3) {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let y = TONE_LUMA_R * r + TONE_LUMA_G * g + TONE_LUMA_B * b;
        pixel[0] = y + (r - y) * scale;
        pixel[1] = y + (g - y) * scale;
        pixel[2] = y + (b - y) * scale;
    }
}

fn default_tone_curve(x: f32) -> f32 {
    if x.is_nan() || x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    if x < TONE_SHADOW_KNEE {
        tone_hermite(
            x,
            0.0,
            TONE_SHADOW_KNEE,
            0.0,
            tone_midtone_line(TONE_SHADOW_KNEE),
            TONE_SHADOW_ENDPOINT_SLOPE,
            TONE_MIDTONE_SLOPE,
        )
    } else if x > TONE_HIGHLIGHT_KNEE {
        tone_hermite(
            x,
            TONE_HIGHLIGHT_KNEE,
            1.0,
            tone_midtone_line(TONE_HIGHLIGHT_KNEE),
            1.0,
            TONE_MIDTONE_SLOPE,
            TONE_HIGHLIGHT_ENDPOINT_SLOPE,
        )
    } else {
        tone_midtone_line(x)
    }
}

fn tone_midtone_line(x: f32) -> f32 {
    TONE_MIDTONE_SLOPE * (x - TONE_MIDTONE_ANCHOR) + TONE_MIDTONE_ANCHOR
}

/// Capture sharpening — mirrors `src/color/sharpen.rs` (Phase 2.5a). Works
/// on luminance only: blur Y, apply the unsharp-mask formula on Y, rescale
/// RGB by `Y_out / Y_in`. Keep params and weights in sync with the real
/// module.
const SHARPEN_SIGMA: f32 = 0.8;
const SHARPEN_AMOUNT: f32 = 0.3;

/// Rec.709 / sRGB luma weights — post-ICC, display-space RGB.
const SHARPEN_LUMA_R: f32 = 0.2126;
const SHARPEN_LUMA_G: f32 = 0.7152;
const SHARPEN_LUMA_B: f32 = 0.0722;

const SHARPEN_DARK_EPSILON: f32 = 1.0e-4;

/// Sharpen a packed RGB8 buffer in place. The real app path uses RGBA8;
/// this example works on RGB8 so we strip the alpha channel altogether.
/// Luminance-only unsharp mask: blur Y, combine, scale RGB.
fn sharpen_rgb8_inplace(rgb: &mut [u8], width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    let pixels = (width as usize) * (height as usize);
    if rgb.len() != pixels * 3 || pixels < 2 {
        return;
    }
    let kernel = sharpen_gaussian_kernel(SHARPEN_SIGMA);
    let radius = kernel.len() / 2;

    let mut luma = Vec::with_capacity(pixels);
    for px in rgb.chunks_exact(3) {
        let r = px[0] as f32;
        let g = px[1] as f32;
        let b = px[2] as f32;
        luma.push(SHARPEN_LUMA_R * r + SHARPEN_LUMA_G * g + SHARPEN_LUMA_B * b);
    }

    let mut tmp = vec![0.0_f32; pixels];
    let mut blurred = vec![0.0_f32; pixels];
    sharpen_blur_h(&luma, &mut tmp, width, height, &kernel, radius);
    sharpen_blur_v(&tmp, &mut blurred, width, height, &kernel, radius);

    for (i, px) in rgb.chunks_exact_mut(3).enumerate() {
        let y_in = luma[i];
        if y_in < SHARPEN_DARK_EPSILON {
            continue;
        }
        let y_out = y_in + (y_in - blurred[i]) * SHARPEN_AMOUNT;
        let scale = y_out / y_in;
        px[0] = ((px[0] as f32 * scale).clamp(0.0, 255.0) + 0.5) as u8;
        px[1] = ((px[1] as f32 * scale).clamp(0.0, 255.0) + 0.5) as u8;
        px[2] = ((px[2] as f32 * scale).clamp(0.0, 255.0) + 0.5) as u8;
    }
}

fn sharpen_gaussian_kernel(sigma: f32) -> Vec<f32> {
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

fn sharpen_blur_h(
    input: &[f32],
    output: &mut [f32],
    width: u32,
    height: u32,
    kernel: &[f32],
    radius: usize,
) {
    let w = width as usize;
    let h = height as usize;
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0_f32;
            for (k_idx, &k) in kernel.iter().enumerate() {
                let offset = k_idx as isize - radius as isize;
                let sx = sharpen_clamp(x as isize + offset, w);
                acc += input[y * w + sx] * k;
            }
            output[y * w + x] = acc;
        }
    }
}

fn sharpen_blur_v(
    input: &[f32],
    output: &mut [f32],
    width: u32,
    height: u32,
    kernel: &[f32],
    radius: usize,
) {
    let w = width as usize;
    let h = height as usize;
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0_f32;
            for (k_idx, &k) in kernel.iter().enumerate() {
                let offset = k_idx as isize - radius as isize;
                let sy = sharpen_clamp(y as isize + offset, h);
                acc += input[sy * w + x] * k;
            }
            output[y * w + x] = acc;
        }
    }
}

fn sharpen_clamp(i: isize, len: usize) -> usize {
    if i < 0 {
        0
    } else if (i as usize) >= len {
        len - 1
    } else {
        i as usize
    }
}

fn tone_hermite(x: f32, x0: f32, x1: f32, y0: f32, y1: f32, m0: f32, m1: f32) -> f32 {
    let dx = x1 - x0;
    let t = (x - x0) / dx;
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    h00 * y0 + h10 * dx * m0 + h01 * y1 + h11 * dx * m1
}

/// Same EV-source priority and clamp as `src/decoding/raw.rs`. Inlined for
/// standalone build.
fn baseline_exposure_ev(decoder: &dyn Decoder, raw: &RawImage) -> f32 {
    if let Some(v) = raw.dng_tags.get(&(DngTag::BaselineExposure as u16))
        && let Some(ev) = tag_value_to_f32(v)
    {
        return clamp_ev(ev);
    }
    if let Ok(Some(ifd)) = decoder.ifd(WellKnownIFD::Root)
        && let Some(entry) = ifd.get_entry(DngTag::BaselineExposure)
        && let Some(ev) = tag_value_to_f32(&entry.value)
    {
        return clamp_ev(ev);
    }
    clamp_ev(DEFAULT_BASELINE_EV)
}

fn clamp_ev(ev: f32) -> f32 {
    if !ev.is_finite() {
        return 0.0;
    }
    ev.clamp(-BASELINE_EV_CLAMP, BASELINE_EV_CLAMP)
}

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
        _ => None,
    }
}

/// Pull an `Intermediate` into a flat RGB f32 buffer. `FourColor` sensors
/// collapse to a 3-channel preview (drop the 4th emerald/etc. channel); the
/// real pipeline keeps them.
fn intermediate_to_rgb_f32(intermediate: &Intermediate) -> (u32, u32, Vec<f32>) {
    match intermediate {
        Intermediate::Monochrome(pixels) => {
            let data = pixels
                .data
                .iter()
                .flat_map(|v| [*v, *v, *v])
                .collect::<Vec<_>>();
            (pixels.width as u32, pixels.height as u32, data)
        }
        Intermediate::ThreeColor(pixels) => {
            let data = pixels.flatten();
            (pixels.width as u32, pixels.height as u32, data)
        }
        Intermediate::FourColor(pixels) => {
            let data = pixels
                .data
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect::<Vec<_>>();
            (pixels.width as u32, pixels.height as u32, data)
        }
    }
}

fn white_balance(raw: &RawImage) -> [f32; 4] {
    if raw.wb_coeffs[0].is_nan() {
        [1.0, 1.0, 1.0, 1.0]
    } else {
        raw.wb_coeffs
    }
}

fn apply_wb_only(rgb: &[f32], wb: &[f32; 4]) -> Vec<f32> {
    rgb.chunks_exact(3)
        .flat_map(|p| [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]])
        .collect()
}

fn camera_to_linear_rec2020(raw: &RawImage, rgb: &[f32], wb: &[f32; 4]) -> Vec<f32> {
    let matrix = raw
        .color_matrix
        .iter()
        .find(|(ill, _)| **ill == Illuminant::D65)
        .or_else(|| raw.color_matrix.iter().next())
        .map(|(_, m)| m.clone())
        .expect("camera has no color matrix");

    if matrix.len() >= 9 {
        // 3x3 path
        let xyz_to_cam = [
            [matrix[0], matrix[1], matrix[2]],
            [matrix[3], matrix[4], matrix[5]],
            [matrix[6], matrix[7], matrix[8]],
        ];
        let rgb2cam = multiply_3x3(&xyz_to_cam, &REC2020_TO_XYZ_D65);
        let rgb2cam = normalize_rows_3(rgb2cam);
        let cam2rgb = invert_3x3(rgb2cam).unwrap_or(IDENTITY_3);
        rgb.chunks_exact(3)
            .flat_map(|p| {
                let r = p[0] * wb[0];
                let g = p[1] * wb[1];
                let b = p[2] * wb[2];
                [
                    cam2rgb[0][0] * r + cam2rgb[0][1] * g + cam2rgb[0][2] * b,
                    cam2rgb[1][0] * r + cam2rgb[1][1] * g + cam2rgb[1][2] * b,
                    cam2rgb[2][0] * r + cam2rgb[2][1] * g + cam2rgb[2][2] * b,
                ]
            })
            .collect()
    } else {
        rgb.to_vec()
    }
}

fn multiply_3x3(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
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

const IDENTITY_3: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn invert_3x3(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
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

/// Preview a linear Rec.2020 buffer as an sRGB-ish PNG so human eyes can see
/// it on a normal display. Out-of-gamut values are clipped here (preview
/// only) and the sRGB gamma is applied. This is lossy; the real pipeline
/// skips this step and hands the f32 buffer to moxcms.
fn linear_to_srgb_preview(rec2020: &[f32]) -> Vec<u8> {
    rec2020
        .iter()
        .map(|v| {
            let v = v.clamp(0.0, 1.0);
            let gamma = if v <= 0.0031308 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (gamma * 255.0 + 0.5) as u8
        })
        .collect()
}

fn f32_to_rgb8(rgb: &[f32]) -> Vec<u8> {
    rgb.iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect()
}

/// For camera-RGB previews: normalise the whole buffer so the brightest pixel
/// lands at 1.0, then scale to 8 bits. Without this, sensor-linear data looks
/// almost pure black on screen.
fn f32_to_rgb8_normalized(rgb: &[f32]) -> Vec<u8> {
    let peak = rgb
        .iter()
        .copied()
        .fold(0.0_f32, |a, b| if b > a { b } else { a });
    let scale = if peak > f32::EPSILON { 1.0 / peak } else { 1.0 };
    rgb.iter()
        .map(|v| ((v * scale).clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect()
}

/// Build the linear Rec.2020 `ColorProfile` (same recipe as the app's
/// `color::profiles::linear_rec2020_profile`, inlined here to keep the
/// example a standalone binary).
fn linear_rec2020_profile() -> ColorProfile {
    let mut profile = ColorProfile::new_bt2020();
    let linear = ToneReprCurve::Lut(Vec::new());
    profile.red_trc = Some(linear.clone());
    profile.green_trc = Some(linear.clone());
    profile.blue_trc = Some(linear);
    profile.description = Some(ProfileText::Localizable(vec![LocalizableString::new(
        "en".to_string(),
        "US".to_string(),
        "Linear Rec.2020 (Prvw)".to_string(),
    )]));
    profile
}

// ----- Opcode application (DNG spec § 6) -----------------------------------
//
// Minimal inline copy of `src/decoding/dng_opcodes.rs` + the pipeline entry
// points in `src/decoding/raw.rs`. Keeps the example a standalone binary; the
// real module has more opcodes and full unit-test coverage.

fn fetch_opcode_list_bytes(decoder: &dyn Decoder, which: DngTag) -> Option<Vec<u8>> {
    let ifd = decoder
        .ifd(WellKnownIFD::VirtualDngRawTags)
        .ok()
        .flatten()?;
    let entry = ifd.get_entry(which)?;
    match &entry.value {
        Value::Byte(bytes) | Value::Undefined(bytes) => Some(bytes.clone()),
        _ => None,
    }
}

fn parse_opcode_count_and_entries(bytes: &[u8]) -> Vec<(u32, u32, usize, Vec<u8>)> {
    // Returns (id, flags, _byte_count, params) tuples for each opcode.
    if bytes.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let mut out = Vec::with_capacity(count);
    let mut cursor = 4;
    for _ in 0..count {
        if cursor + 16 > bytes.len() {
            break;
        }
        let id = u32::from_be_bytes([
            bytes[cursor],
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
        ]);
        let flags = u32::from_be_bytes([
            bytes[cursor + 8],
            bytes[cursor + 9],
            bytes[cursor + 10],
            bytes[cursor + 11],
        ]);
        let len = u32::from_be_bytes([
            bytes[cursor + 12],
            bytes[cursor + 13],
            bytes[cursor + 14],
            bytes[cursor + 15],
        ]) as usize;
        cursor += 16;
        if cursor + len > bytes.len() {
            break;
        }
        out.push((id, flags, len, bytes[cursor..cursor + len].to_vec()));
        cursor += len;
    }
    out
}

/// Apply the `OpcodeList1`/`OpcodeList2` opcodes on a CFA-mosaic `RawImage`.
/// Only `GainMap` is supported in this example; other opcodes are skipped.
fn apply_opcode_list(decoder: &dyn Decoder, raw: &mut RawImage, tag: DngTag, label: &str) {
    let Some(bytes) = fetch_opcode_list_bytes(decoder, tag) else {
        return;
    };
    let entries = parse_opcode_count_and_entries(&bytes);
    if entries.is_empty() {
        return;
    }
    println!("  {label}: {} opcode(s)", entries.len());
    let width = raw.width as u32;
    let height = raw.height as u32;
    let cpp = raw.cpp;
    let was_integer = matches!(raw.data, RawImageData::Integer(_));
    let mut data = raw.data.as_f32().into_owned();
    for (id, _flags, _len, params) in &entries {
        if *id == 9
            && let Some(map) = parse_gain_map_params(params)
            && cpp == 1
        {
            // CFA photometric = one plane per DNG spec § 6.2.2. Bayer-
            // phase selection is spatial (rect + pitch), not per-color.
            apply_gain_map_cfa(&mut data, width, height, &map);
        }
    }
    if was_integer {
        let as_u16: Vec<u16> = data
            .iter()
            .map(|v| (v.clamp(0.0, 1.0) * u16::MAX as f32) as u16)
            .collect();
        raw.data = RawImageData::Integer(as_u16);
    } else {
        raw.data = RawImageData::Float(data);
    }
}

/// Apply `OpcodeList3` opcodes on a 3-channel RGB buffer. Returns `true`
/// when a `WarpRectilinear` fired (signals to the caller that Phase 4
/// lens correction should be skipped).
fn apply_opcode_list3_rgb(
    decoder: &dyn Decoder,
    width: u32,
    height: u32,
    rec2020: &mut [f32],
) -> bool {
    let Some(bytes) = fetch_opcode_list_bytes(decoder, DngTag::OpcodeList3) else {
        return false;
    };
    let entries = parse_opcode_count_and_entries(&bytes);
    if entries.is_empty() {
        return false;
    }
    println!("  OpcodeList3: {} opcode(s)", entries.len());
    let mut warp_applied = false;
    for (id, _flags, _len, params) in &entries {
        if *id == 1
            && let Some(warp) = parse_warp_rectilinear_params(params)
        {
            apply_warp_rectilinear_rgb(rec2020, width, height, &warp);
            warp_applied = true;
        }
    }
    warp_applied
}

/// Phase 4 lens correction: look up camera + lens in LensFun and apply
/// distortion + TCA + vignetting. Mirrors the logic in
/// `src/color/lens_correction.rs`; inlined so the example stays a
/// standalone binary. Returns `true` when at least one pass fired.
#[allow(clippy::too_many_arguments)]
fn apply_lens_correction_stage(
    raw: &RawImage,
    lens_model: &str,
    focal: f32,
    aperture: f32,
    distance: f32,
    rgb: &mut [f32],
    width: u32,
    height: u32,
) -> bool {
    use lensfun::{Database, Modifier};
    use std::sync::OnceLock;
    static DB: OnceLock<Option<Database>> = OnceLock::new();
    let Some(db) = DB.get_or_init(|| Database::load_bundled().ok()).as_ref() else {
        return false;
    };
    let cameras = db.find_cameras(Some(&raw.camera.make), &raw.camera.model);
    let Some(camera) = cameras.first().copied() else {
        return false;
    };
    let lenses = db.find_lenses(Some(camera), lens_model);
    let Some(lens) = lenses.first().copied() else {
        return false;
    };
    println!(
        "  lens_correction: matched '{}' ({}mm f/{}) on '{} {}'",
        lens.model, focal, aperture, camera.maker, camera.model
    );
    let mut modifier = Modifier::new(lens, focal, camera.crop_factor, width, height, true);
    let d = modifier.enable_distortion_correction(lens);
    let t = modifier.enable_tca_correction(lens);
    let v = modifier.enable_vignetting_correction(lens, aperture, distance);
    if !(d || t || v) {
        return false;
    }
    if v {
        modifier.apply_color_modification_f32(rgb, 0.0, 0.0, width as usize, height as usize, 3);
    }
    let w = width as usize;
    let h = height as usize;
    if d {
        let src = rgb.to_vec();
        let mut coords = vec![0.0_f32; w * 2];
        for y in 0..h {
            modifier.apply_geometry_distortion(0.0, y as f32, w, 1, &mut coords);
            for x in 0..w {
                let (r, g, b) =
                    sample_rgb_bilinear_inline(&src, w, h, coords[2 * x], coords[2 * x + 1]);
                let off = (y * w + x) * 3;
                rgb[off] = r;
                rgb[off + 1] = g;
                rgb[off + 2] = b;
            }
        }
    }
    if t {
        let src = rgb.to_vec();
        let mut coords = vec![0.0_f32; w * 6];
        for y in 0..h {
            modifier.apply_subpixel_distortion(0.0, y as f32, w, 1, &mut coords);
            for x in 0..w {
                let base = x * 6;
                let (r, _, _) =
                    sample_rgb_bilinear_inline(&src, w, h, coords[base], coords[base + 1]);
                let (_, g, _) =
                    sample_rgb_bilinear_inline(&src, w, h, coords[base + 2], coords[base + 3]);
                let (_, _, b) =
                    sample_rgb_bilinear_inline(&src, w, h, coords[base + 4], coords[base + 5]);
                let off = (y * w + x) * 3;
                rgb[off] = r;
                rgb[off + 1] = g;
                rgb[off + 2] = b;
            }
        }
    }
    d || t || v
}

fn sample_rgb_bilinear_inline(
    src: &[f32],
    w: usize,
    h: usize,
    sx: f32,
    sy: f32,
) -> (f32, f32, f32) {
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

#[derive(Debug, Clone)]
struct GainMapInline {
    top: u32,
    left: u32,
    bottom: u32,
    right: u32,
    /// Parsed for spec completeness. On CFA photometric input (the only
    /// case `apply_gain_map_cfa` is called with here) there's one plane
    /// per DNG spec § 6.2.2, so this is always 0.
    #[allow(dead_code)]
    plane: u32,
    row_pitch: u32,
    col_pitch: u32,
    map_points_v: u32,
    map_points_h: u32,
    map_spacing_v: f64,
    map_spacing_h: f64,
    map_origin_v: f64,
    map_origin_h: f64,
    gains: Vec<f32>,
}

fn parse_gain_map_params(params: &[u8]) -> Option<GainMapInline> {
    if params.len() < 76 {
        return None;
    }
    let u = |o: usize| u32::from_be_bytes([params[o], params[o + 1], params[o + 2], params[o + 3]]);
    let d = |o: usize| {
        f64::from_be_bytes([
            params[o],
            params[o + 1],
            params[o + 2],
            params[o + 3],
            params[o + 4],
            params[o + 5],
            params[o + 6],
            params[o + 7],
        ])
    };
    let map_planes = u(72).max(1);
    let map_points_v = u(32);
    let map_points_h = u(36);
    let gain_count = (map_points_v as usize) * (map_points_h as usize) * (map_planes as usize);
    if params.len() < 76 + gain_count * 4 {
        return None;
    }
    let mut gains = Vec::with_capacity(gain_count);
    for i in 0..gain_count {
        let o = 76 + i * 4;
        gains.push(f32::from_be_bytes([
            params[o],
            params[o + 1],
            params[o + 2],
            params[o + 3],
        ]));
    }
    Some(GainMapInline {
        top: u(0),
        left: u(4),
        bottom: u(8),
        right: u(12),
        plane: u(16),
        row_pitch: u(24).max(1),
        col_pitch: u(28).max(1),
        map_points_v,
        map_points_h,
        map_spacing_v: d(40),
        map_spacing_h: d(48),
        map_origin_v: d(56),
        map_origin_h: d(64),
        gains,
    })
}

/// CFA GainMap apply. Per DNG spec § 6.2.2, CFA photometric data has one
/// plane; Bayer-phase selection is spatial (rect + pitch), not per-color.
/// Keep in sync with `src/decoding/dng_opcodes.rs::apply_gain_map_cfa`.
fn apply_gain_map_cfa(data: &mut [f32], width: u32, height: u32, map: &GainMapInline) {
    let w = width as usize;
    if data.len() != w * (height as usize) {
        return;
    }
    let rect_h = map.right.saturating_sub(map.left).max(1) as f64;
    let rect_v = map.bottom.saturating_sub(map.top).max(1) as f64;
    let sample = |v: f64, h: f64| -> f32 {
        let pv = map.map_points_v.max(1) as f64;
        let ph = map.map_points_h.max(1) as f64;
        let fy =
            ((v - map.map_origin_v) / map.map_spacing_v.max(f64::EPSILON)).clamp(0.0, pv - 1.0);
        let fx =
            ((h - map.map_origin_h) / map.map_spacing_h.max(f64::EPSILON)).clamp(0.0, ph - 1.0);
        let y0 = fy.floor() as usize;
        let x0 = fx.floor() as usize;
        let y1 = (y0 + 1).min(map.map_points_v.saturating_sub(1) as usize);
        let x1 = (x0 + 1).min(map.map_points_h.saturating_sub(1) as usize);
        let ty = (fy - y0 as f64) as f32;
        let tx = (fx - x0 as f64) as f32;
        let stride = map.map_points_h as usize;
        let idx = |y: usize, x: usize| y * stride + x;
        let g00 = map.gains[idx(y0, x0)];
        let g01 = map.gains[idx(y0, x1)];
        let g10 = map.gains[idx(y1, x0)];
        let g11 = map.gains[idx(y1, x1)];
        let g0 = g00 * (1.0 - tx) + g01 * tx;
        let g1 = g10 * (1.0 - tx) + g11 * tx;
        g0 * (1.0 - ty) + g1 * ty
    };
    for y in 0..height {
        if y < map.top || y >= map.bottom {
            continue;
        }
        if !(y - map.top).is_multiple_of(map.row_pitch) {
            continue;
        }
        let v_norm = (y - map.top) as f64 / rect_v;
        for x in 0..width {
            if x < map.left || x >= map.right {
                continue;
            }
            if !(x - map.left).is_multiple_of(map.col_pitch) {
                continue;
            }
            let h_norm = (x - map.left) as f64 / rect_h;
            let gain = sample(v_norm, h_norm);
            data[y as usize * w + x as usize] *= gain;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct WarpPlaneInline {
    kr0: f64,
    kr1: f64,
    kr2: f64,
    kr3: f64,
    kt0: f64,
    kt1: f64,
    cx: f64,
    cy: f64,
}

fn parse_warp_rectilinear_params(params: &[u8]) -> Option<Vec<WarpPlaneInline>> {
    if params.len() < 4 {
        return None;
    }
    let plane_count = u32::from_be_bytes([params[0], params[1], params[2], params[3]]) as usize;
    if plane_count == 0 || plane_count > 4 {
        return None;
    }
    if params.len() < 4 + plane_count * 64 {
        return None;
    }
    let d = |o: usize| {
        f64::from_be_bytes([
            params[o],
            params[o + 1],
            params[o + 2],
            params[o + 3],
            params[o + 4],
            params[o + 5],
            params[o + 6],
            params[o + 7],
        ])
    };
    let mut out = Vec::with_capacity(plane_count);
    let mut cursor = 4;
    for _ in 0..plane_count {
        out.push(WarpPlaneInline {
            kr0: d(cursor),
            kr1: d(cursor + 8),
            kr2: d(cursor + 16),
            kr3: d(cursor + 24),
            kt0: d(cursor + 32),
            kt1: d(cursor + 40),
            cx: d(cursor + 48),
            cy: d(cursor + 56),
        });
        cursor += 64;
    }
    Some(out)
}

fn apply_warp_rectilinear_rgb(data: &mut [f32], width: u32, height: u32, warp: &[WarpPlaneInline]) {
    let w = width as usize;
    let h = height as usize;
    if data.len() != w * h * 3 || warp.is_empty() {
        return;
    }
    let source = data.to_vec();
    let half_w = (w as f64 - 1.0) * 0.5;
    let half_h = (h as f64 - 1.0) * 0.5;
    let norm = (half_w * half_w + half_h * half_h).sqrt().max(1.0);
    let sample = |sx: f64, sy: f64, plane: usize| -> f32 {
        if !sx.is_finite() || !sy.is_finite() {
            return 0.0;
        }
        let max_x = (w as f64 - 1.0).max(0.0);
        let max_y = (h as f64 - 1.0).max(0.0);
        let x = sx.clamp(0.0, max_x);
        let y = sy.clamp(0.0, max_y);
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = (x0 + 1).min(w - 1);
        let y1 = (y0 + 1).min(h - 1);
        let tx = (x - x0 as f64) as f32;
        let ty = (y - y0 as f64) as f32;
        let at = |yy: usize, xx: usize| source[(yy * w + xx) * 3 + plane];
        let a = at(y0, x0) * (1.0 - tx) + at(y0, x1) * tx;
        let b = at(y1, x0) * (1.0 - tx) + at(y1, x1) * tx;
        a * (1.0 - ty) + b * ty
    };
    for y in 0..h {
        for x in 0..w {
            let chunk_off = (y * w + x) * 3;
            for plane_idx in 0..3 {
                let plane = if warp.len() == 1 {
                    warp[0]
                } else {
                    warp[plane_idx.min(warp.len() - 1)]
                };
                let cx_pix = plane.cx * (w as f64 - 1.0);
                let cy_pix = plane.cy * (h as f64 - 1.0);
                let dx = (x as f64 - cx_pix) / norm;
                let dy = (y as f64 - cy_pix) / norm;
                let r2 = dx * dx + dy * dy;
                let r4 = r2 * r2;
                let r6 = r4 * r2;
                let radial = plane.kr0 + plane.kr1 * r2 + plane.kr2 * r4 + plane.kr3 * r6;
                let sx_rel =
                    dx * radial + 2.0 * plane.kt0 * dx * dy + plane.kt1 * (r2 + 2.0 * dx * dx);
                let sy_rel =
                    dy * radial + plane.kt0 * (r2 + 2.0 * dy * dy) + 2.0 * plane.kt1 * dx * dy;
                let sx = sx_rel * norm + cx_pix;
                let sy = sy_rel * norm + cy_pix;
                data[chunk_off + plane_idx] = sample(sx, sy, plane_idx);
            }
        }
    }
}

/// Preview a CFA-mosaic (cpp=1) buffer as a grayscale PNG at roughly
/// perceptual brightness. Used for `after-opcode1/2` stages.
fn cfa_preview(raw: &RawImage) -> Vec<u8> {
    let w = raw.width;
    let h = raw.height;
    let data = raw.data.as_f32();
    let peak = data
        .iter()
        .copied()
        .fold(0.0_f32, |a, b| if b > a { b } else { a });
    let scale = if peak > f32::EPSILON { 1.0 / peak } else { 1.0 };
    let mut rgb = Vec::with_capacity(w * h * 3);
    for v in data.iter() {
        let lum = (v * scale).clamp(0.0, 1.0);
        // Apply sRGB gamma for eyeballable brightness.
        let gamma = if lum <= 0.0031308 {
            lum * 12.92
        } else {
            1.055 * lum.powf(1.0 / 2.4) - 0.055
        };
        let byte = (gamma * 255.0 + 0.5) as u8;
        rgb.push(byte);
        rgb.push(byte);
        rgb.push(byte);
    }
    rgb
}

fn transform_f32_rec2020_to_srgb(rgb: &mut [f32]) {
    let srgb_bytes = std::fs::read(SRGB_PROFILE_PATH).expect("system sRGB profile missing");
    let target = ColorProfile::new_from_slice(&srgb_bytes).expect("couldn't parse sRGB profile");
    let source = linear_rec2020_profile();
    let options = TransformOptions {
        rendering_intent: RenderingIntent::Perceptual,
        ..TransformOptions::default()
    };
    let transform: std::sync::Arc<dyn InPlaceTransformExecutor<f32> + Send + Sync> = source
        .create_in_place_transform_f32(Layout::Rgb, &target, options)
        .expect("couldn't build ICC transform");
    transform.transform(rgb).expect("ICC transform failed");
}

/// Write an RGB8 buffer as a PNG.
fn save_rgb_png(
    path: &Path,
    width: u32,
    height: u32,
    rgb: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let buf: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_raw(width, height, rgb.to_vec()).ok_or("RGB buffer size mismatch")?;
    buf.save(path)?;
    Ok(())
}
