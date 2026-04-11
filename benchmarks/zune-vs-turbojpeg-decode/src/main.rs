use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rand::seq::SliceRandom;
use rand::rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Scenario {
    ZuneJpeg,
    TurbojpegFull,
    TurbojpegHalf,
    TurbojpegQuarter,
    TurbojpegEighth,
}

impl Scenario {
    const ALL: [Scenario; 5] = [
        Scenario::ZuneJpeg,
        Scenario::TurbojpegFull,
        Scenario::TurbojpegHalf,
        Scenario::TurbojpegQuarter,
        Scenario::TurbojpegEighth,
    ];

    fn label(&self) -> &'static str {
        match self {
            Scenario::ZuneJpeg => "zune-jpeg",
            Scenario::TurbojpegFull => "turbojpeg/full",
            Scenario::TurbojpegHalf => "turbojpeg/1:2",
            Scenario::TurbojpegQuarter => "turbojpeg/1:4",
            Scenario::TurbojpegEighth => "turbojpeg/1:8",
        }
    }

    fn turbojpeg_scale(&self) -> Option<turbojpeg::ScalingFactor> {
        match self {
            Scenario::ZuneJpeg => None,
            Scenario::TurbojpegFull => Some(turbojpeg::ScalingFactor::ONE),
            Scenario::TurbojpegHalf => Some(turbojpeg::ScalingFactor::ONE_HALF),
            Scenario::TurbojpegQuarter => Some(turbojpeg::ScalingFactor::ONE_QUARTER),
            Scenario::TurbojpegEighth => Some(turbojpeg::ScalingFactor::ONE_EIGHTH),
        }
    }
}

struct ImageData {
    path: PathBuf,
    data: Vec<u8>,
    width: usize,
    height: usize,
}

const RUNS_PER_PAIR: usize = 3;

fn decode_zune(data: &[u8]) {
    use zune_core::bytestream::ZCursor;
    use zune_core::options::DecoderOptions;
    use zune_jpeg::JpegDecoder;

    let options = DecoderOptions::new_fast();
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    decoder.decode().expect("zune-jpeg decode failed");
}

fn decode_turbojpeg(data: &[u8], scale: turbojpeg::ScalingFactor) -> (usize, usize) {
    let mut decompressor = turbojpeg::Decompressor::new().expect("failed to create decompressor");
    let header = decompressor.read_header(data).expect("failed to read header");

    decompressor
        .set_scaling_factor(scale)
        .expect("failed to set scaling factor");

    let scaled_width = scale.scale(header.width);
    let scaled_height = scale.scale(header.height);
    let format = turbojpeg::PixelFormat::RGB;
    let pitch = scaled_width * format.size();

    let mut image = turbojpeg::Image {
        pixels: vec![0u8; scaled_height * pitch],
        width: scaled_width,
        pitch,
        height: scaled_height,
        format,
    };

    decompressor
        .decompress(data, image.as_deref_mut())
        .expect("turbojpeg decompress failed");

    (scaled_width, scaled_height)
}

fn format_duration(mean: Duration, stddev: Duration) -> String {
    let mean_ms = mean.as_secs_f64() * 1000.0;
    let std_ms = stddev.as_secs_f64() * 1000.0;
    format!("{mean_ms:.1}\u{00b1}{std_ms:.1}ms")
}

fn format_file_size(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    format!("{mb:.1} MB")
}

fn compute_stats(durations: &[Duration]) -> (Duration, Duration) {
    let n = durations.len() as f64;
    let sum: Duration = durations.iter().sum();
    let mean = sum / durations.len() as u32;
    let mean_secs = mean.as_secs_f64();

    let variance = durations
        .iter()
        .map(|d| {
            let diff = d.as_secs_f64() - mean_secs;
            diff * diff
        })
        .sum::<f64>()
        / n;
    let stddev = Duration::from_secs_f64(variance.sqrt());

    (mean, stddev)
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: jpeg-decode-bench <image1.jpg> [image2.jpg ...]");
        std::process::exit(1);
    }

    // Load all images into memory
    let mut images: Vec<ImageData> = Vec::new();
    for path_str in &args {
        let path = PathBuf::from(path_str);
        let data = fs::read(&path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
        let file_size = data.len();

        // Read dimensions via turbojpeg header
        let mut decompressor = turbojpeg::Decompressor::new().expect("failed to create decompressor");
        let header = decompressor
            .read_header(&data)
            .expect("failed to read JPEG header");

        println!(
            "Loaded {} ({}\u{00d7}{}, {})",
            path.file_name().unwrap().to_string_lossy(),
            header.width,
            header.height,
            format_file_size(file_size as u64),
        );

        images.push(ImageData {
            path,
            data,
            width: header.width,
            height: header.height,
        });
    }

    // Build all (image_index, scenario, run_index) tuples and shuffle
    let mut tasks: Vec<(usize, Scenario, usize)> = Vec::new();
    for img_idx in 0..images.len() {
        for scenario in &Scenario::ALL {
            for run in 0..RUNS_PER_PAIR {
                tasks.push((img_idx, *scenario, run));
            }
        }
    }
    tasks.shuffle(&mut rng());

    let total = tasks.len();
    println!("\nRunning {total} benchmark tasks ({} images \u{00d7} {} scenarios \u{00d7} {RUNS_PER_PAIR} runs, randomized)...\n",
        images.len(), Scenario::ALL.len());

    // Results: (image_index, scenario) -> Vec<Duration>
    let mut results: HashMap<(usize, Scenario), Vec<Duration>> = HashMap::new();
    // Output dimensions for turbojpeg scenarios: (image_index, scenario) -> (width, height)
    let mut output_dims: HashMap<(usize, Scenario), (usize, usize)> = HashMap::new();

    for (i, (img_idx, scenario, _run)) in tasks.iter().enumerate() {
        let img = &images[*img_idx];

        if (i + 1) % 20 == 0 || i == 0 {
            print!("  [{}/{}]\r", i + 1, total);
        }

        let start = Instant::now();
        match scenario {
            Scenario::ZuneJpeg => {
                decode_zune(&img.data);
            }
            _ => {
                let scale = scenario.turbojpeg_scale().unwrap();
                let dims = decode_turbojpeg(&img.data, scale);
                output_dims.insert((*img_idx, *scenario), dims);
            }
        }
        let elapsed = start.elapsed();

        results
            .entry((*img_idx, *scenario))
            .or_default()
            .push(elapsed);
    }

    // Print results
    println!("\n");

    // Print output dimensions for each scale factor (using first image as reference)
    if !images.is_empty() {
        let img = &images[0];
        println!("Output dimensions (using {}x{} as reference):", img.width, img.height);
        for scenario in &Scenario::ALL {
            let dims = match scenario {
                Scenario::ZuneJpeg => (img.width, img.height),
                _ => output_dims
                    .get(&(0, *scenario))
                    .copied()
                    .unwrap_or((0, 0)),
            };
            println!("  {:16} -> {}x{}", scenario.label(), dims.0, dims.1);
        }
        println!();
    }

    // Column widths
    let col_w = 16;

    // Header
    print!("{:<15} {:>14} {:>10}", "Image", "Dimensions", "File size");
    for scenario in &Scenario::ALL {
        print!("  {:>col_w$}", scenario.label());
    }
    println!();

    // Separator
    let total_width = 15 + 14 + 10 + (col_w + 2) * Scenario::ALL.len();
    println!("{}", "-".repeat(total_width));

    // Per-scenario averages across all images
    let mut scenario_all_means: HashMap<Scenario, Vec<f64>> = HashMap::new();

    for (img_idx, img) in images.iter().enumerate() {
        let file_size = img.data.len() as u64;
        let filename = img.path.file_name().unwrap().to_string_lossy();
        print!(
            "{:<15} {:>6}x{:<6} {:>10}",
            filename,
            img.width,
            img.height,
            format_file_size(file_size),
        );

        for scenario in &Scenario::ALL {
            if let Some(durations) = results.get(&(img_idx, *scenario)) {
                let (mean, stddev) = compute_stats(durations);
                print!("  {:>col_w$}", format_duration(mean, stddev));
                scenario_all_means
                    .entry(*scenario)
                    .or_default()
                    .push(mean.as_secs_f64() * 1000.0);
            } else {
                print!("  {:>col_w$}", "N/A");
            }
        }
        println!();
    }

    // Averages row
    println!("{}", "-".repeat(total_width));
    print!("{:<15} {:>14} {:>10}", "Averages", "", "");
    for scenario in &Scenario::ALL {
        if let Some(means) = scenario_all_means.get(scenario) {
            let n = means.len() as f64;
            let avg = means.iter().sum::<f64>() / n;
            let variance = means.iter().map(|m| (m - avg).powi(2)).sum::<f64>() / n;
            let stddev = variance.sqrt();
            print!("  {:>col_w$}", format!("{avg:.1}\u{00b1}{stddev:.1}ms"));
        } else {
            print!("  {:>col_w$}", "N/A");
        }
    }
    println!();
}
