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
//! - `final` — the RGBA8 buffer Prvw actually renders, after ICC transform to
//!   sRGB (or whatever `--target-icc` points at).
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

    // Stage 5: final ICC-transformed sRGB output, same as what Prvw ships on
    // an sRGB display.
    let t0 = Instant::now();
    let mut rec2020_for_icc = rec2020_lifted;
    transform_f32_rec2020_to_srgb(&mut rec2020_for_icc);
    let final_rgb = f32_to_rgb8(&rec2020_for_icc);
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
