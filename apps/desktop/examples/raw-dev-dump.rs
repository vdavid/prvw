//! Per-stage RAW pipeline inspector.
//!
//! Takes a RAW file and dumps labeled PNGs for each meaningful pipeline stage.
//! Current stages:
//!
//! - `post-demosaic` — rawler's sensor-level output: rescale + demosaic +
//!   active-area crop only. Camera-native RGB, pre-white-balance. Looks green
//!   and dark because WB and the camera matrix haven't landed yet.
//! - `post-wb` — same buffer, with white-balance coefficients applied.
//! - `linear-rec2020` — after our `cam → XYZ → linear Rec.2020` matrix. Still
//!   wide-gamut linear; PNG-encoded with an sRGB-like gamma so you can eyeball
//!   it on a normal display. Values outside sRGB are clipped in the preview
//!   only — the real pipeline keeps them.
//! - `post-exposure` — linear Rec.2020 after the baseline-exposure lift
//!   (Phase 2.2). Same sRGB-ish preview encoding as `linear-rec2020` so
//!   side-by-side brightness changes are eyeballable.
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
    let raw = decoder.raw_image(&src, &params, false)?;

    // Stage 1: sensor-level develop (rescale + demosaic + active-area crop).
    let t0 = Instant::now();
    let develop = RawDevelop {
        steps: vec![
            ProcessingStep::Rescale,
            ProcessingStep::Demosaic,
            ProcessingStep::CropActiveArea,
        ],
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
    let rec2020 = camera_to_linear_rec2020(&raw, &demosaic_rgb_f32, &wb);
    let linear_preview = linear_to_srgb_preview(&rec2020);
    let linear_rec2020_ms = t0.elapsed().as_millis();

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

    // Stage 5: default tone curve (Phase 2.3 / 2.5a). Mild filmic S-curve
    // shaped on luminance only; every pixel's RGB is scaled uniformly by
    // `Y_out / Y_in`. Same sRGB-ish preview encoding so the added contrast
    // is eyeballable against `post-exposure`.
    let t0 = Instant::now();
    let mut rec2020_toned = rec2020_lifted.clone();
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
            name: "post-exposure",
            width: demosaic_w,
            height: demosaic_h,
            rgb: post_exposure_preview,
            took_ms: post_exposure_ms,
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
const SATURATION_BOOST: f32 = 0.00;

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
