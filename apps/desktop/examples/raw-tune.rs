//! Empirical grid-search tuner for the RAW pipeline's Phase 2 parameters.
//!
//! Takes a RAW file plus a `sips` PNG reference of the same file, runs the
//! Prvw decode pipeline once to produce a pre-tone linear Rec.2020 buffer,
//! then sweeps every combination in a three-parameter grid (tone curve
//! midtone anchor, sharpen amount, saturation boost) for the remaining
//! pipeline stages. Each combo's output is compared to the reference via
//! CIE76 Delta-E; the tool prints a ranked table of the top 10 combos and
//! writes the single best combo's output as `best.png`.
//!
//! Decode-once / tune-many is deliberate: the expensive stages (rawler
//! demosaic, WB + camera matrix, exposure, default crop) don't depend on
//! the three tuned parameters, so running them once and then sweeping the
//! cheap post-tone stages keeps the grid fast (~200 ms per combo vs. ~1.8 s
//! per combo if we re-decoded every time).
//!
//! ## Usage
//!
//! Single-file mode:
//!
//! ```sh
//! cd apps/desktop
//! cargo run --release --example raw-tune -- \
//!     --raw /tmp/raw/sample1.arw \
//!     --reference /tmp/prvw-tune/sample1-sips.png \
//!     --out-dir /tmp/prvw-tune-sample1
//! ```
//!
//! Multi-file cross-validation (the ranking uses mean Delta-E across every
//! file, so a combo that wins on one but loses on another drops down the
//! table):
//!
//! ```sh
//! cargo run --release --example raw-tune -- \
//!     --raw /tmp/raw/sample1.arw \
//!     --raw /tmp/raw/sample2.dng \
//!     --raw /tmp/raw/sample3.arw \
//!     --reference /tmp/prvw-tune/sample1-sips.png \
//!     --reference /tmp/prvw-tune/sample2-sips.png \
//!     --reference /tmp/prvw-tune/sample3-sips.png \
//!     --out-dir /tmp/prvw-tune-cross
//! ```
//!
//! ## Reference dimensions
//!
//! Two reference kinds are supported:
//!
//! 1. **`sips` PNG exports.** Match the RAW's pre-orientation decoded
//!    dimensions. Sony ARW files pass straight through; iPhone ProRAW DNGs
//!    carry an EXIF rotation that `sips` applies to the export, so the
//!    tuner rotates the reference back 90° to match our pre-orientation
//!    buffer. Exact dimension match (or a clean 90° rotation) is required.
//!
//! 2. **Preview.app screenshots.** CleanShot / screenshot captures of
//!    Preview.app rendering a RAW at fit-to-window zoom. Screenshots are
//!    always smaller than the decoded buffer; the tuner **downsamples the
//!    decoded output with Lanczos3** to match the screenshot's dimensions
//!    before running Delta-E. Downsampling (not upsampling the screenshot)
//!    keeps the Delta-E metric honest: upsampling a screenshot invents
//!    detail that biases toward fuzzy output. A warning fires and bilinear
//!    upsampling falls back if the reference is somehow larger than the
//!    decoded buffer (shouldn't happen in practice).
//!
//! ## Relationship to the production modules
//!
//! The tone curve, saturation, and sharpen math below mirrors
//! `src/color/{tone_curve, saturation, sharpen}.rs`. Cargo's example
//! targets don't have access to the binary crate's internals, so the
//! math is duplicated here. Same approach as `raw-dev-dump.rs`. Keep the
//! constants and loop shapes in sync when the real modules change.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use image::imageops::FilterType;
use image::{DynamicImage, ImageBuffer, Rgb, Rgba, RgbaImage};
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
use rayon::prelude::*;

/// Keep in sync with `src/decoding/raw.rs::DEFAULT_BASELINE_EV`.
const DEFAULT_BASELINE_EV: f32 = 0.5;

/// Keep in sync with `src/decoding/raw.rs::BASELINE_EV_CLAMP`.
const BASELINE_EV_CLAMP: f32 = 2.0;

/// Rec.2020 D65 → XYZ matrix. Keep in sync with
/// `src/color/profiles.rs::REC2020_TO_XYZ_D65`.
#[allow(clippy::excessive_precision)]
const REC2020_TO_XYZ_D65: [[f32; 3]; 3] = [
    [0.6369580, 0.1446169, 0.1688810],
    [0.2627002, 0.6779981, 0.0593017],
    [0.0000000, 0.0280727, 1.0609851],
];

/// Tone curve shape constants. Keep in sync with
/// `src/color/tone_curve.rs`. Only `DEFAULT_MIDTONE_ANCHOR` is sweepable;
/// every other curve parameter is a shape-defining constant that we don't
/// tune here.
const TONE_SHADOW_KNEE: f32 = 0.10;
const TONE_HIGHLIGHT_KNEE: f32 = 0.90;
const TONE_MIDTONE_SLOPE: f32 = 1.08;
const TONE_SHADOW_ENDPOINT_SLOPE: f32 = 1.0;
const TONE_HIGHLIGHT_ENDPOINT_SLOPE: f32 = 0.30;
const TONE_DARK_EPSILON: f32 = 1.0e-5;

/// Rec.2020 luma coefficients — tone curve + saturation run in linear
/// Rec.2020. Keep in sync with `src/color/tone_curve.rs`.
const REC2020_LUMA_R: f32 = 0.2627;
const REC2020_LUMA_G: f32 = 0.6780;
const REC2020_LUMA_B: f32 = 0.0593;

/// Sharpen σ. Keep in sync with `src/color/sharpen.rs::DEFAULT_SIGMA`.
const SHARPEN_SIGMA: f32 = 0.8;

/// Rec.709 / sRGB luma weights — sharpening runs post-ICC in display
/// space. Keep in sync with `src/color/sharpen.rs`.
const SHARPEN_LUMA_R: f32 = 0.2126;
const SHARPEN_LUMA_G: f32 = 0.7152;
const SHARPEN_LUMA_B: f32 = 0.0722;

const SHARPEN_DARK_EPSILON: f32 = 1.0e-4;

const SRGB_PROFILE_PATH: &str = "/System/Library/ColorSync/Profiles/sRGB Profile.icc";

const DEFAULT_ANCHORS: &[f32] = &[0.25, 0.30, 0.35, 0.40, 0.45, 0.50];
const DEFAULT_AMOUNTS: &[f32] = &[0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65];
const DEFAULT_BOOSTS: &[f32] = &[0.00, 0.05, 0.10, 0.15, 0.20, 0.25];

#[derive(Parser, Debug)]
#[command(about = "Grid-search the Phase 2 RAW parameters against one or more sips references")]
struct Args {
    /// Path to a source RAW file (ARW, DNG, CR2, etc.). Pass `--raw`
    /// multiple times for cross-validation across several photos.
    #[arg(long, required = true)]
    raw: Vec<PathBuf>,

    /// Path to a reference PNG (typically a `sips` export of the same
    /// RAW). Must be provided once per `--raw`, in the same order.
    #[arg(long, required = true)]
    reference: Vec<PathBuf>,

    /// Output directory. Receives `best-<stem>.png` per input plus a
    /// `cross-validation.csv` summary when more than one file is used.
    #[arg(long)]
    out_dir: PathBuf,

    /// Comma-separated tone-curve midtone anchors to sweep.
    #[arg(long, value_delimiter = ',')]
    anchors: Option<Vec<f32>>,

    /// Comma-separated sharpen amounts to sweep.
    #[arg(long, value_delimiter = ',')]
    amounts: Option<Vec<f32>>,

    /// Comma-separated saturation boosts to sweep.
    #[arg(long, value_delimiter = ',')]
    boosts: Option<Vec<f32>>,

    /// Maximum rows to print to stdout. CSVs always list every combo.
    #[arg(long, default_value_t = 10)]
    top_n: usize,
}

#[derive(Debug, Clone, Copy)]
struct Combo {
    anchor: f32,
    amount: f32,
    boost: f32,
}

#[derive(Debug, Clone, Copy)]
struct DeltaE {
    mean: f32,
    max: f32,
    p95: f32,
}

/// A loaded reference image with its native dimensions. The Delta-E metric
/// runs at these dimensions: if the reference is a Preview.app screenshot
/// smaller than the decoded buffer, the evaluator downsamples our output to
/// match before scoring.
struct Reference {
    width: u32,
    height: u32,
    /// RGBA8 at `width × height`. Length is always `width * height * 4`.
    rgba: Vec<u8>,
}

struct Input {
    path: PathBuf,
    shared: SharedDecode,
    reference: Reference,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    std::fs::create_dir_all(&args.out_dir)?;

    if args.raw.len() != args.reference.len() {
        return Err(format!(
            "expected one --reference per --raw; got {} raw and {} reference",
            args.raw.len(),
            args.reference.len(),
        )
        .into());
    }

    let anchors: Vec<f32> = args
        .anchors
        .clone()
        .unwrap_or_else(|| DEFAULT_ANCHORS.to_vec());
    let amounts: Vec<f32> = args
        .amounts
        .clone()
        .unwrap_or_else(|| DEFAULT_AMOUNTS.to_vec());
    let boosts: Vec<f32> = args
        .boosts
        .clone()
        .unwrap_or_else(|| DEFAULT_BOOSTS.to_vec());

    println!("Out dir  : {}", args.out_dir.display());
    println!(
        "Grid     : {} anchors × {} amounts × {} boosts = {} combos",
        anchors.len(),
        amounts.len(),
        boosts.len(),
        anchors.len() * amounts.len() * boosts.len(),
    );
    println!("Inputs   :");
    for (raw, reference) in args.raw.iter().zip(args.reference.iter()) {
        println!("  raw={} reference={}", raw.display(), reference.display());
    }

    // Prebuild the ICC transform once. Every combo and every input reuses it.
    let srgb_bytes = std::fs::read(SRGB_PROFILE_PATH)?;
    let target = ColorProfile::new_from_slice(&srgb_bytes)?;
    let source = linear_rec2020_profile();
    let icc: Arc<dyn InPlaceTransformExecutor<f32> + Send + Sync> = source
        .create_in_place_transform_f32(
            Layout::Rgb,
            &target,
            TransformOptions {
                rendering_intent: RenderingIntent::Perceptual,
                ..TransformOptions::default()
            },
        )?;

    // Decode each RAW once and load its reference. Collect into a `Vec`
    // so the grid can hit every file per combo.
    let mut inputs: Vec<Input> = Vec::with_capacity(args.raw.len());
    for (raw_path, ref_path) in args.raw.iter().zip(args.reference.iter()) {
        let t_decode = Instant::now();
        let shared = decode_shared(raw_path)?;
        let reference = load_and_match_reference(ref_path, shared.width, shared.height)?;
        println!(
            "Decoded  : {} ({} ms)  decoded={}x{}  reference={}x{}",
            raw_path.display(),
            t_decode.elapsed().as_millis(),
            shared.width,
            shared.height,
            reference.width,
            reference.height,
        );
        inputs.push(Input {
            path: raw_path.clone(),
            shared,
            reference,
        });
    }

    // Build the combo grid. Parallel evaluate across (combo × input), then
    // aggregate the per-input Delta-E stats into a single ranking score
    // (mean across inputs).
    let mut combos: Vec<Combo> = Vec::with_capacity(anchors.len() * amounts.len() * boosts.len());
    for &anchor in &anchors {
        for &amount in &amounts {
            for &boost in &boosts {
                combos.push(Combo {
                    anchor,
                    amount,
                    boost,
                });
            }
        }
    }

    let t_grid = Instant::now();
    // Each entry: (combo, Vec<DeltaE> per input).
    let mut per_combo: Vec<(Combo, Vec<DeltaE>)> = combos
        .par_iter()
        .map(|&combo| {
            let mut stats = Vec::with_capacity(inputs.len());
            for input in &inputs {
                let rgba = evaluate_combo(&input.shared, combo, &icc);
                let resized = resize_to_reference(&rgba, &input.shared, &input.reference);
                stats.push(delta_e_stats(&input.reference.rgba, &resized));
            }
            (combo, stats)
        })
        .collect();
    let grid_ms = t_grid.elapsed().as_millis();
    let total_evals = combos.len() * inputs.len();
    println!(
        "Grid     : {} ms for {} combos × {} files = {} evaluations ({:.1} ms/eval)",
        grid_ms,
        combos.len(),
        inputs.len(),
        total_evals,
        grid_ms as f32 / total_evals as f32,
    );

    // Rank by mean-of-means Delta-E across inputs.
    per_combo.sort_by(|a, b| {
        mean_of_means(&a.1)
            .partial_cmp(&mean_of_means(&b.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    println!();
    print_cross_top_table(&per_combo, &inputs, args.top_n);

    write_cross_csv(
        &args.out_dir.join("cross-validation.csv"),
        &per_combo,
        &inputs,
    )?;

    // Write best.png per input using the overall-winner combo.
    let (winning_combo, winning_stats) = per_combo.first().ok_or("grid was empty")?;
    println!(
        "\nOverall winner: anchor {:.2}  amount {:.2}  boost {:+.2}  mean-of-means ΔE {:.3}",
        winning_combo.anchor,
        winning_combo.amount,
        winning_combo.boost,
        mean_of_means(winning_stats),
    );
    for (input, stats) in inputs.iter().zip(winning_stats.iter()) {
        let stem = input
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("input");
        let rgba = evaluate_combo(&input.shared, *winning_combo, &icc);
        let path = args.out_dir.join(format!("best-{stem}.png"));
        save_rgba_png(&path, input.shared.width, input.shared.height, &rgba)?;
        println!(
            "  {}  mean {:.3}  max {:.1}  p95 {:.2}  → {}",
            stem,
            stats.mean,
            stats.max,
            stats.p95,
            path.display(),
        );
    }

    Ok(())
}

/// Mean across per-input Delta-E means. The ranking score for cross-
/// validation: a combo that wins on one file but loses on another lands
/// lower than one that's consistently close.
fn mean_of_means(stats: &[DeltaE]) -> f32 {
    if stats.is_empty() {
        return f32::INFINITY;
    }
    let sum: f32 = stats.iter().map(|s| s.mean).sum();
    sum / stats.len() as f32
}

/// Material shared across every grid combo: the full-res pre-tone buffer
/// in linear Rec.2020, plus the final dimensions (post-default-crop).
struct SharedDecode {
    width: u32,
    height: u32,
    /// Flat RGB f32, length `width * height * 3`. Post-exposure, pre-tone.
    rec2020: Vec<f32>,
}

/// Decode the RAW through stages that don't depend on the tuned parameters:
/// demosaic, white balance, camera matrix → linear Rec.2020, default crop,
/// baseline exposure. Returns the full-res pre-tone buffer.
fn decode_shared(path: &Path) -> Result<SharedDecode, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let src = RawSource::new_from_shared_vec(Arc::new(bytes)).with_path(path);
    let decoder = rawler::get_decoder(&src)?;
    let params = RawDecodeParams::default();
    let raw = decoder.raw_image(&src, &params, false)?;

    let develop = RawDevelop {
        steps: vec![
            ProcessingStep::Rescale,
            ProcessingStep::Demosaic,
            ProcessingStep::CropActiveArea,
        ],
    };
    let intermediate = develop.develop_intermediate(&raw)?;
    let (width, height, cam_rgb) = intermediate_to_rgb_f32(&intermediate);
    let wb = white_balance(&raw);
    let rec2020 = camera_to_linear_rec2020(&raw, &cam_rgb, &wb);
    let (width, height, mut rec2020) = apply_default_crop(&raw, width, height, rec2020);

    let ev = baseline_exposure_ev(decoder.as_ref(), &raw);
    println!("Baseline EV : {:+.2}", ev);
    apply_exposure(&mut rec2020, ev);

    Ok(SharedDecode {
        width,
        height,
        rec2020,
    })
}

/// Run a single combo through the remaining pipeline: tone curve,
/// saturation, ICC, RGBA8 quantise, sharpen. Returns the finished RGBA8
/// buffer (length `width * height * 4`) ready for Delta-E comparison.
fn evaluate_combo(
    shared: &SharedDecode,
    combo: Combo,
    icc: &Arc<dyn InPlaceTransformExecutor<f32> + Send + Sync>,
) -> Vec<u8> {
    let mut rec2020 = shared.rec2020.clone();
    apply_tone_curve(&mut rec2020, combo.anchor);
    apply_saturation_boost(&mut rec2020, combo.boost);
    icc.transform(&mut rec2020).expect("ICC transform failed");
    let mut rgba = rec2020_to_rgba8(&rec2020);
    sharpen_rgba8_inplace(
        &mut rgba,
        shared.width,
        shared.height,
        SHARPEN_SIGMA,
        combo.amount,
    );
    rgba
}

/// Load the reference PNG at its native dimensions. Three cases handled:
///
/// 1. Dimensions match the decoded buffer exactly → return as-is.
/// 2. Dimensions match after a 90° CW rotation → rotate, return. Covers
///    iPhone ProRAW DNGs where `sips` applies EXIF orientation but our
///    pre-orientation buffer is at sensor-native layout.
/// 3. Otherwise → return at the reference's own dimensions. The evaluator
///    resamples our decoded output to this size before scoring. Catches
///    Preview.app screenshot references, which are always smaller than the
///    decoded RAW (CleanShot fit-to-window zoom ≈ 2/3 resolution).
fn load_and_match_reference(
    path: &Path,
    target_w: u32,
    target_h: u32,
) -> Result<Reference, Box<dyn std::error::Error>> {
    let img = image::open(path)?.to_rgba8();
    let (w, h) = (img.width(), img.height());

    if (w, h) == (target_w, target_h) {
        return Ok(Reference {
            width: w,
            height: h,
            rgba: img.into_raw(),
        });
    }
    if (h, w) == (target_w, target_h) {
        let rotated = image::imageops::rotate90(&img);
        return Ok(Reference {
            width: rotated.width(),
            height: rotated.height(),
            rgba: rotated.into_raw(),
        });
    }

    // Fall through: reference dimensions don't match. Accept anyway — the
    // evaluator downsamples the decoded output to the reference's size.
    // We just log the ratio so a badly-cropped reference is easy to spot.
    let ratio_w = w as f32 / target_w as f32;
    let ratio_h = h as f32 / target_h as f32;
    println!(
        "Reference {}: {}x{} vs decoded {}x{} (scale {:.3}x{:.3}). \
         Output will be resampled to match before Delta-E.",
        path.display(),
        w,
        h,
        target_w,
        target_h,
        ratio_w,
        ratio_h,
    );
    if (ratio_w - ratio_h).abs() > 0.01 {
        return Err(format!(
            "Reference {w}x{h} and decoded {target_w}x{target_h} have \
             non-uniform scale ({ratio_w:.3} vs {ratio_h:.3}). Aspect ratios \
             differ, so one axis would stretch. Fix the reference before retrying.",
        )
        .into());
    }
    Ok(Reference {
        width: w,
        height: h,
        rgba: img.into_raw(),
    })
}

/// Bring the decoded RGBA8 output to the reference's dimensions. Three
/// cases:
///
/// - Exact match → return a clone (no-op resize).
/// - Output larger than reference → downsample with Lanczos3. Preview.app
///   screenshots land here.
/// - Output smaller than reference → warn and upsample with bilinear.
///   Shouldn't happen in normal use; we don't panic because it's still
///   useful to eyeball an out-of-shape reference.
fn resize_to_reference(rgba: &[u8], shared: &SharedDecode, reference: &Reference) -> Vec<u8> {
    if shared.width == reference.width && shared.height == reference.height {
        return rgba.to_vec();
    }
    let src: RgbaImage =
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(shared.width, shared.height, rgba.to_vec())
            .expect("rgba length must equal width * height * 4");
    let src = DynamicImage::ImageRgba8(src);
    let filter = if shared.width >= reference.width && shared.height >= reference.height {
        FilterType::Lanczos3
    } else {
        // Upsampling our output would invent detail that biases Delta-E
        // downward. Bilinear is the least-biased filter for this case;
        // worth a one-line log so the dev notices.
        eprintln!(
            "warn: decoded {}x{} smaller than reference {}x{} — upsampling with bilinear",
            shared.width, shared.height, reference.width, reference.height,
        );
        FilterType::Triangle
    };
    let resized = src.resize_exact(reference.width, reference.height, filter);
    resized.to_rgba8().into_raw()
}

/// Print a ranked top-N table with the overall score plus per-input
/// mean Delta-E. Compact enough to eyeball across 3–5 inputs.
fn print_cross_top_table(per_combo: &[(Combo, Vec<DeltaE>)], inputs: &[Input], top_n: usize) {
    print!(
        "{:>4}  {:>6}  {:>6}  {:>6}  {:>10}",
        "rank", "anchor", "amount", "boost", "mean-of-m",
    );
    for input in inputs {
        let stem = input
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("input");
        print!("  {:>10}", stem);
    }
    println!();
    for (rank, (combo, stats)) in per_combo.iter().take(top_n).enumerate() {
        print!(
            "{:>4}  {:>6.2}  {:>6.2}  {:>+6.2}  {:>10.3}",
            rank + 1,
            combo.anchor,
            combo.amount,
            combo.boost,
            mean_of_means(stats),
        );
        for s in stats {
            print!("  {:>10.3}", s.mean);
        }
        println!();
    }
}

fn write_cross_csv(
    path: &Path,
    per_combo: &[(Combo, Vec<DeltaE>)],
    inputs: &[Input],
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    write!(f, "rank,anchor,amount,boost,mean_of_means")?;
    for input in inputs {
        let stem = input
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("input");
        write!(f, ",{stem}_mean,{stem}_max,{stem}_p95")?;
    }
    writeln!(f)?;
    for (rank, (combo, stats)) in per_combo.iter().enumerate() {
        write!(
            f,
            "{},{:.2},{:.2},{:.2},{:.4}",
            rank + 1,
            combo.anchor,
            combo.amount,
            combo.boost,
            mean_of_means(stats),
        )?;
        for s in stats {
            write!(f, ",{:.4},{:.3},{:.3}", s.mean, s.max, s.p95)?;
        }
        writeln!(f)?;
    }
    Ok(())
}

// ======================================================================
// Color math — mirrors `src/color/tone_curve.rs`,
// `src/color/saturation.rs`, and `src/color/sharpen.rs`. Kept inline so
// this example stays a standalone binary. Same approach as
// `raw-dev-dump.rs`. Keep in sync when the real modules change.
// ======================================================================

/// Parametric tone curve — mirrors
/// `src/color/tone_curve.rs::apply_tone_curve`. Luminance-only apply:
/// `Y_out = curve(Y_in, anchor)`, scale RGB by `Y_out / Y_in`.
fn apply_tone_curve(rgb: &mut [f32], midtone_anchor: f32) {
    rgb.par_chunks_exact_mut(3).for_each(|pixel| {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let y_in = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;
        if !y_in.is_finite() || y_in < TONE_DARK_EPSILON {
            pixel[0] = 0.0;
            pixel[1] = 0.0;
            pixel[2] = 0.0;
            return;
        }
        let y_out = tone_curve(y_in, midtone_anchor);
        let scale = y_out / y_in;
        pixel[0] = r * scale;
        pixel[1] = g * scale;
        pixel[2] = b * scale;
    });
}

fn tone_curve(x: f32, midtone_anchor: f32) -> f32 {
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
            tone_midtone_line(TONE_SHADOW_KNEE, midtone_anchor),
            TONE_SHADOW_ENDPOINT_SLOPE,
            TONE_MIDTONE_SLOPE,
        )
    } else if x > TONE_HIGHLIGHT_KNEE {
        tone_hermite(
            x,
            TONE_HIGHLIGHT_KNEE,
            1.0,
            tone_midtone_line(TONE_HIGHLIGHT_KNEE, midtone_anchor),
            1.0,
            TONE_MIDTONE_SLOPE,
            TONE_HIGHLIGHT_ENDPOINT_SLOPE,
        )
    } else {
        tone_midtone_line(x, midtone_anchor)
    }
}

fn tone_midtone_line(x: f32, midtone_anchor: f32) -> f32 {
    TONE_MIDTONE_SLOPE * (x - midtone_anchor) + midtone_anchor
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

/// Mirror of `src/color/saturation.rs::apply_saturation_boost`.
fn apply_saturation_boost(rgb: &mut [f32], boost: f32) {
    if boost == 0.0 {
        return;
    }
    let scale = 1.0 + boost;
    rgb.par_chunks_exact_mut(3).for_each(|pixel| {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        let y = REC2020_LUMA_R * r + REC2020_LUMA_G * g + REC2020_LUMA_B * b;
        pixel[0] = y + (r - y) * scale;
        pixel[1] = y + (g - y) * scale;
        pixel[2] = y + (b - y) * scale;
    });
}

/// Mirror of `src/color/sharpen.rs::sharpen_rgba8_inplace_with`.
fn sharpen_rgba8_inplace(rgba: &mut [u8], width: u32, height: u32, sigma: f32, amount: f32) {
    if width == 0 || height == 0 {
        return;
    }
    let pixels = (width as usize) * (height as usize);
    if rgba.len() != pixels * 4 || pixels < 2 || amount == 0.0 {
        return;
    }
    let kernel = gaussian_kernel_1d(sigma);
    let radius = kernel.len() / 2;

    let mut luma_in = vec![0.0_f32; pixels];
    luma_in
        .par_iter_mut()
        .zip(rgba.par_chunks_exact(4))
        .for_each(|(slot, px)| {
            let r = px[0] as f32;
            let g = px[1] as f32;
            let b = px[2] as f32;
            *slot = SHARPEN_LUMA_R * r + SHARPEN_LUMA_G * g + SHARPEN_LUMA_B * b;
        });

    let mut scratch = vec![0.0_f32; pixels];
    let mut blurred = vec![0.0_f32; pixels];
    blur_horizontal(&luma_in, &mut scratch, width, height, &kernel, radius);
    blur_vertical(&scratch, &mut blurred, width, height, &kernel, radius);

    rgba.par_chunks_exact_mut(4)
        .zip(luma_in.par_iter())
        .zip(blurred.par_iter())
        .for_each(|((px, &y_in), &y_blurred)| {
            if y_in < SHARPEN_DARK_EPSILON {
                return;
            }
            let y_out = y_in + (y_in - y_blurred) * amount;
            let scale = y_out / y_in;
            px[0] = ((px[0] as f32 * scale).clamp(0.0, 255.0) + 0.5) as u8;
            px[1] = ((px[1] as f32 * scale).clamp(0.0, 255.0) + 0.5) as u8;
            px[2] = ((px[2] as f32 * scale).clamp(0.0, 255.0) + 0.5) as u8;
        });
}

fn gaussian_kernel_1d(sigma: f32) -> Vec<f32> {
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

fn blur_horizontal(
    input: &[f32],
    output: &mut [f32],
    width: u32,
    height: u32,
    kernel: &[f32],
    radius: usize,
) {
    let w = width as usize;
    let _ = height;
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

fn clamp_index(i: isize, len: usize) -> usize {
    if i < 0 {
        0
    } else if (i as usize) >= len {
        len - 1
    } else {
        i as usize
    }
}

// ======================================================================
// CIE76 Delta-E — mirrors `src/color/delta_e.rs`. Parallelised over pixel
// chunks via rayon because a 20 MP buffer has ~20M pixels to score per
// combo and the default serial loop dominates the grid runtime.
// ======================================================================

fn delta_e_stats(a: &[u8], b: &[u8]) -> DeltaE {
    assert_eq!(a.len(), b.len(), "buffer lengths must match");
    let count = a.len() / 4;
    if count == 0 {
        return DeltaE {
            mean: 0.0,
            max: 0.0,
            p95: 0.0,
        };
    }

    // Parallel scan: sum per-chunk, reduce.
    let deltas: Vec<f32> = a
        .par_chunks_exact(4)
        .zip(b.par_chunks_exact(4))
        .map(|(pa, pb)| delta_e_cie76([pa[0], pa[1], pa[2]], [pb[0], pb[1], pb[2]]))
        .collect();

    let sum: f64 = deltas.par_iter().map(|&d| d as f64).sum();
    let max = deltas.par_iter().copied().reduce(|| 0.0_f32, f32::max);
    let mean = (sum / count as f64) as f32;

    // p95: `select_nth_unstable_by` is O(n), vs. ~O(n log n) for a full
    // sort. On 20 MP buffers that's ~5× faster. We don't need the full
    // ordering — just the value at the p95 index.
    let mut partial = deltas;
    let p95_idx = ((count as f32) * 0.95).ceil() as usize;
    let p95_idx = p95_idx.min(count - 1);
    let (_, nth, _) = partial.select_nth_unstable_by(p95_idx, |x, y| {
        x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
    });
    let p95 = *nth;

    DeltaE { mean, max, p95 }
}

fn delta_e_cie76(a: [u8; 3], b: [u8; 3]) -> f32 {
    let la = srgb8_to_lab(a);
    let lb = srgb8_to_lab(b);
    let dl = la[0] - lb[0];
    let da = la[1] - lb[1];
    let db = la[2] - lb[2];
    (dl * dl + da * da + db * db).sqrt()
}

fn srgb8_to_lab(p: [u8; 3]) -> [f32; 3] {
    let lin = [
        srgb_to_linear(p[0] as f32 / 255.0),
        srgb_to_linear(p[1] as f32 / 255.0),
        srgb_to_linear(p[2] as f32 / 255.0),
    ];
    let xyz = linear_rgb_to_xyz(lin);
    xyz_to_lab(xyz)
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_rgb_to_xyz(rgb: [f32; 3]) -> [f32; 3] {
    let r = rgb[0];
    let g = rgb[1];
    let b = rgb[2];
    [
        0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b,
        0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b,
        0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b,
    ]
}

fn xyz_to_lab(xyz: [f32; 3]) -> [f32; 3] {
    const XN: f32 = 0.950_47;
    const YN: f32 = 1.0;
    const ZN: f32 = 1.088_83;
    let fx = lab_f(xyz[0] / XN);
    let fy = lab_f(xyz[1] / YN);
    let fz = lab_f(xyz[2] / ZN);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_f(t: f32) -> f32 {
    const DELTA3: f32 = 0.008_856_452;
    const K: f32 = 7.787_037;
    if t > DELTA3 {
        t.cbrt()
    } else {
        K * t + 16.0 / 116.0
    }
}

// ======================================================================
// RAW pipeline helpers — same as `raw-dev-dump.rs`. Kept inline for
// binary-standalone reasons. Keep in sync with `src/decoding/raw.rs`.
// ======================================================================

fn apply_exposure(rec2020: &mut [f32], ev: f32) {
    if ev == 0.0 {
        return;
    }
    let gain = 2.0_f32.powf(ev);
    rec2020.par_iter_mut().for_each(|v| *v *= gain);
}

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

fn camera_to_linear_rec2020(raw: &RawImage, rgb: &[f32], wb: &[f32; 4]) -> Vec<f32> {
    let matrix = raw
        .color_matrix
        .iter()
        .find(|(ill, _)| **ill == Illuminant::D65)
        .or_else(|| raw.color_matrix.iter().next())
        .map(|(_, m)| m.clone())
        .expect("camera has no color matrix");

    if matrix.len() >= 9 {
        let xyz_to_cam = [
            [matrix[0], matrix[1], matrix[2]],
            [matrix[3], matrix[4], matrix[5]],
            [matrix[6], matrix[7], matrix[8]],
        ];
        let rgb2cam = multiply_3x3(&xyz_to_cam, &REC2020_TO_XYZ_D65);
        let rgb2cam = normalize_rows_3(rgb2cam);
        let cam2rgb = invert_3x3(rgb2cam).unwrap_or(IDENTITY_3);
        rgb.par_chunks_exact(3)
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

/// Build the linear Rec.2020 `ColorProfile`. Mirrors
/// `src/color/profiles.rs::linear_rec2020_profile`.
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

fn save_rgba_png(
    path: &Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    // Strip alpha to RGB for the PNG — the viewer produces fully-opaque
    // pixels so the alpha adds no signal.
    let mut rgb: Vec<u8> = Vec::with_capacity((width * height * 3) as usize);
    for chunk in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&chunk[..3]);
    }
    let buf: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_raw(width, height, rgb).ok_or("RGB buffer size mismatch")?;
    buf.save(path)?;
    Ok(())
}
