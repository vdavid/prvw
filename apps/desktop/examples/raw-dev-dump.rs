//! Per-stage RAW pipeline inspector.
//!
//! Takes a RAW file and dumps labeled PNGs for each meaningful pipeline stage.
//! Phase 1 has two stages: `post-rawler.png` (rawler's default develop output,
//! what the app currently ships) and `final.png` (same as `post-rawler` in
//! Phase 1). Later phases add `linear-widegamut`, `post-exposure`, `post-tone`,
//! `post-sharpen`, and so on, by inserting new `Stage` entries between
//! `post-rawler` and `final`.
//!
//! ## Usage
//!
//! ```sh
//! cd apps/desktop
//! cargo run --example raw-dev-dump -- path/to/file.dng
//! cargo run --example raw-dev-dump -- file.arw --out-dir /tmp/my-dump
//! ```
//!
//! Output defaults to `/tmp/prvw-dev-dump-<filename>/`. The example runs
//! through rawler's develop pipeline directly (not the app's `load_image`),
//! so it bypasses ICC transform and orientation — we want to see what the
//! pipeline produces, not what ends up on screen.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use image::{ImageBuffer, Rgb};
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::RawDevelop;
use rawler::rawsource::RawSource;

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

/// Run the full pipeline and collect a stage per meaningful output. Phase 1
/// has `post-rawler` and `final`; Phase 2 will grow this list.
fn run_pipeline(path: &Path) -> Result<Vec<Stage>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let src = RawSource::new_from_shared_vec(Arc::new(bytes)).with_path(path);

    let t0 = Instant::now();
    let decoder = rawler::get_decoder(&src)?;
    let params = RawDecodeParams::default();
    let raw = decoder.raw_image(&src, &params, false)?;
    let intermediate = RawDevelop::default().develop_intermediate(&raw)?;
    let dyn_img = intermediate
        .to_dynamic_image()
        .ok_or("develop produced no image")?;
    let rgb = dyn_img.to_rgb8();
    let (width, height) = (rgb.width(), rgb.height());
    let rgb_bytes = rgb.into_raw();
    let post_rawler_ms = t0.elapsed().as_millis();

    // Phase 1: `final` is the same as `post-rawler`. Phase 2.x will grow the
    // pipeline between these two entries.
    let stages = vec![
        Stage {
            name: "post-rawler",
            width,
            height,
            rgb: rgb_bytes.clone(),
            took_ms: post_rawler_ms,
        },
        Stage {
            name: "final",
            width,
            height,
            rgb: rgb_bytes,
            took_ms: 0,
        },
    ];

    Ok(stages)
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
