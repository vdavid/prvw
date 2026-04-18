//! # Decoding
//!
//! Image format decoders. JPEG via `zune-jpeg` (SIMD); PNG, GIF, WebP, BMP, TIFF via the
//! `image` crate; camera RAW (ARW, CR2, CR3, DNG, NEF, ORF, PEF, RAF, RW2, SRW) via
//! `rawler`. Also extracts the embedded ICC profile (transform lives in `crate::color`).
//!
//! ## Key choices
//!
//! - **`zune-jpeg` for JPEG** — significantly faster than the `image` crate's JPEG path on
//!   Apple Silicon. Used unconditionally for JPEGs.
//! - **`image` crate for everything else non-RAW** — mature, covers the rest.
//! - **`rawler` for RAW** — runs its built-in develop pipeline (demosaic, white balance,
//!   color matrix, sRGB gamma) in one call, parallelised via rayon.
//! - **Cancellation.** `load_image_cancellable` takes an `AtomicBool` — checked at format
//!   entry and between decode stages. The preloader uses this so navigating away aborts
//!   in-flight work before it finishes a wasted decode.
//!
//! ## Public API
//!
//! - [`DecodedImage`] — pixel buffer plus dimensions, ready for GPU upload.
//!   Pixels are either RGBA8 (every non-RAW format, plus SDR RAW output) or
//!   RGBA16F (RAW output when HDR is active and the display can display it).
//! - [`load_image`] / [`load_image_cancellable`] — decode a file to `DecodedImage`,
//!   color-managed to a target ICC profile, with EXIF orientation applied.
//! - [`is_supported_extension`] — format gate used by the directory scanner.

mod dispatch;
mod dng_opcodes;
mod generic;
mod jpeg;
mod orientation;
mod raw;
mod raw_flags;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use dispatch::Backend;
use orientation::{apply_orientation_bytes, parse_exif_orientation};

pub use raw_flags::{
    MIDTONE_ANCHOR_RANGE, RawPipelineFlags, SATURATION_BOOST_RANGE, SHARPEN_AMOUNT_RANGE,
};

/// Pixel-buffer variants. `Rgba8` is `[r, g, b, a, r, g, b, a, …]` in sRGB
/// gamma-encoded bytes — the common case. `Rgba16F` is `[r, g, b, a, r, …]`
/// where every element is the IEEE 754 half-precision float bit pattern
/// stored as `u16` (use the `half` crate to convert to `f32`). Half-float
/// RGBA is only produced by the RAW decoder when HDR output is active.
pub enum PixelBuffer {
    Rgba8(Vec<u8>),
    Rgba16F(Vec<u16>),
}

impl PixelBuffer {
    /// Bytes per pixel for cache-size accounting and GPU row-pitch math.
    /// RGBA8 is 4 bytes per pixel; RGBA16F is 8 bytes per pixel (four u16s).
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            PixelBuffer::Rgba8(_) => 4,
            PixelBuffer::Rgba16F(_) => 8,
        }
    }

    /// Total byte length of the pixel buffer. Multiply `bytes_per_pixel()`
    /// by pixel count and you get this; kept as a helper so callers don't
    /// need to know which variant they have.
    pub fn byte_len(&self) -> usize {
        match self {
            PixelBuffer::Rgba8(v) => v.len(),
            PixelBuffer::Rgba16F(v) => v.len() * 2,
        }
    }

    /// True when the backing storage is RGBA16F (half-float).
    pub fn is_hdr(&self) -> bool {
        matches!(self, PixelBuffer::Rgba16F(_))
    }
}

/// Decoded image data ready for GPU upload.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: PixelBuffer,
}

impl DecodedImage {
    /// Build an RGBA8 image. Kept around so the JPEG / PNG / WebP / etc.
    /// paths don't have to know the `PixelBuffer` enum exists.
    pub fn from_rgba8(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels: PixelBuffer::Rgba8(rgba),
        }
    }

    /// Build an RGBA16F image. Used by `decoding::raw` when HDR output is
    /// active. `half_rgba` is 4 × width × height `u16`s in RGBA order, each
    /// element an IEEE 754 half-precision bit pattern.
    pub fn from_rgba16f(width: u32, height: u32, half_rgba: Vec<u16>) -> Self {
        Self {
            width,
            height,
            pixels: PixelBuffer::Rgba16F(half_rgba),
        }
    }
}

/// Decode an image file to a `DecodedImage`, color-managed to the given
/// target ICC profile. JPEGs use zune-jpeg (SIMD-accelerated). RAW files use
/// rawler. Everything else goes through the `image` crate. Applies EXIF
/// orientation correction automatically. Images without an embedded ICC
/// profile are assumed sRGB and transformed to `target_icc`.
///
/// `edr_headroom` is the peak-white headroom the display can show (use
/// [`crate::color::display_profile::current_edr_headroom`] on macOS). `1.0`
/// means "SDR only — clip highlights at display-white". Anything above
/// `1.0` combined with `raw_flags.hdr_output == true` triggers the
/// `RGBA16F` output path for RAW files.
pub fn load_image(
    path: &Path,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    raw_flags: RawPipelineFlags,
    edr_headroom: f32,
) -> Result<DecodedImage, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

    log::debug!("Loading {}", path.display());
    let start = Instant::now();

    let bytes =
        std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;

    let backend = dispatch::pick_backend(ext);
    let result = decode_with(
        backend,
        path,
        filename,
        bytes,
        None,
        target_icc,
        use_relative_colorimetric,
        raw_flags,
        edr_headroom,
    );

    log_result(&result, ext, backend, path, start);
    result
}

/// Decode an image file, with cancellation support. See [`load_image`] for
/// the `edr_headroom` contract.
pub fn load_image_cancellable(
    path: &Path,
    cancelled: &AtomicBool,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    raw_flags: RawPipelineFlags,
    edr_headroom: f32,
) -> Result<DecodedImage, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

    log::debug!("Loading (cancellable) {}", path.display());
    let start = Instant::now();

    let bytes = read_file_cancellable(path, cancelled)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }

    let backend = dispatch::pick_backend(ext);
    let result = decode_with(
        backend,
        path,
        filename,
        bytes,
        Some(cancelled),
        target_icc,
        use_relative_colorimetric,
        raw_flags,
        edr_headroom,
    );

    log_result(&result, ext, backend, path, start);
    result
}

/// Check if a file extension is a supported image format.
pub fn is_supported_extension(ext: &str) -> bool {
    dispatch::is_supported_extension(ext)
}

/// Dispatch to the chosen backend. JPEG and Generic parse EXIF orientation from the
/// outer file bytes; Raw gets orientation from rawler's decoder metadata instead
/// (rawler always sets `RawImage.orientation` to Normal).
#[allow(clippy::too_many_arguments)] // Internal dispatch; plumbing trumps struct-ifying
fn decode_with(
    backend: Backend,
    path: &Path,
    filename: &str,
    bytes: Vec<u8>,
    cancelled: Option<&AtomicBool>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    raw_flags: RawPipelineFlags,
    edr_headroom: f32,
) -> Result<DecodedImage, String> {
    match backend {
        Backend::Jpeg => {
            let orientation = parse_exif_orientation(&bytes, filename);
            let img = jpeg::decode(path, bytes, target_icc, use_relative_colorimetric)?;
            Ok(finalize(img, orientation))
        }
        Backend::Generic => {
            let orientation = parse_exif_orientation(&bytes, filename);
            let img = generic::decode(path, bytes, target_icc, use_relative_colorimetric)?;
            Ok(finalize(img, orientation))
        }
        Backend::Raw => {
            let (img, orientation) = raw::decode(
                path,
                bytes,
                cancelled,
                target_icc,
                use_relative_colorimetric,
                raw_flags,
                edr_headroom,
            )?;
            if orientation != 1 {
                log::debug!("RAW orientation: {orientation} for {filename}");
            }
            Ok(finalize(img, orientation))
        }
    }
}

/// Apply EXIF orientation and update dimensions. Works on both RGBA8 and
/// RGBA16F — the rotation logic is a per-pixel block swap, so it factors
/// across the `bytes_per_pixel()` stride cleanly. For RGBA16F we operate
/// on the underlying `u16` slice directly, treating each "pixel" as a
/// 4-element block (one per RGBA channel).
fn finalize(mut img: DecodedImage, orientation: u16) -> DecodedImage {
    let (old_w, old_h) = (img.width, img.height);
    let (new_w, new_h) = match &mut img.pixels {
        PixelBuffer::Rgba8(bytes) => apply_orientation_bytes(old_w, old_h, bytes, orientation, 4),
        PixelBuffer::Rgba16F(halfs) => {
            orientation::apply_orientation_u16(old_w, old_h, halfs, orientation, 4)
        }
    };
    if (new_w, new_h) != (old_w, old_h) {
        log::debug!(
            "Applied rotation: orientation {orientation} ({old_w}x{old_h} -> {new_w}x{new_h})"
        );
    }
    img.width = new_w;
    img.height = new_h;
    img
}

/// Read a file in 64 KB chunks, checking a cancellation flag between chunks.
fn read_file_cancellable(path: &Path, cancelled: &AtomicBool) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let size = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
    let mut buf = Vec::with_capacity(size);
    let mut chunk = [0u8; 65536];
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = file
            .read(&mut chunk)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

/// Shared success/failure logging for both entry points.
fn log_result(
    result: &Result<DecodedImage, String>,
    ext: &str,
    backend: Backend,
    path: &Path,
    start: Instant,
) {
    match result {
        Ok(image) => {
            let duration = start.elapsed();
            let decoded_size = format_decoded_size(image.pixels.byte_len());
            let format_name = match backend {
                Backend::Jpeg => "JPEG via zune-jpeg".to_string(),
                Backend::Raw => format!("{} via rawler", ext.to_uppercase()),
                Backend::Generic => ext.to_uppercase(),
            };
            let hdr_label = if image.pixels.is_hdr() { " [HDR]" } else { "" };
            log::info!(
                "Decoded {format_name}{hdr_label}: {}x{} ({decoded_size}) in {}ms",
                image.width,
                image.height,
                duration.as_millis()
            );
        }
        Err(msg) if msg == "cancelled" => {
            log::debug!("Cancelled loading {}", path.display());
        }
        Err(msg) => {
            log::warn!("Decode failed for {}: {msg}", path.display());
        }
    }
}

/// Format a byte count as a compact human-readable string (for example, "47.2 MB").
fn format_decoded_size(bytes: usize) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.1} KB", b / 1024.0)
    }
}

// Gated to macOS because `color::srgb_icc_bytes` reads a macOS-only system
// profile path. All tests in this block are `#[ignore]`'d anyway, but the gate
// keeps Linux CI from panicking on test discovery.
#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::color;

    /// End-to-end: `load_image` on the ARW fixture. Verifies dimensions after
    /// orientation, which for sample1 is no-op (orientation 1). `#[ignore]` because
    /// the fixture lives outside the repo. Run with
    /// `cargo test decoding::tests::arw_end_to_end -- --ignored`.
    #[test]
    #[ignore]
    fn arw_end_to_end() {
        let path = Path::new("/tmp/raw/sample1.arw");
        let img = load_image(
            path,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
            1.0, // SDR headroom — keep the fixture path RGBA8 for golden diffs
        )
        .expect("decode failed");
        assert_eq!((img.width, img.height), (5456, 3632));
    }

    /// End-to-end: `load_image` on the DNG fixture. sample2 comes out of rawler
    /// as 3990x3000 but carries EXIF orientation 6 or 8, which swaps dims to
    /// 3000x3990.
    #[test]
    #[ignore]
    fn dng_end_to_end() {
        let path = Path::new("/tmp/raw/sample2.dng");
        let img = load_image(
            path,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
            1.0, // SDR headroom — keep the fixture path RGBA8 for golden diffs
        )
        .expect("decode failed");
        assert_eq!((img.width, img.height), (3000, 3990));
    }

    /// Golden regression test: decode the synthetic Bayer DNG fixture via the
    /// full `load_image` path and compare against a checked-in golden PNG. The
    /// threshold is deliberately tight (mean < 0.5, max < 3.0 in CIE76 Delta-E)
    /// so any pipeline drift caught by Phase 2+ changes will trip this test.
    ///
    /// To regenerate after an intentional output change:
    ///   PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden
    ///
    /// The fixture is a 128x128 uncompressed Bayer RGGB DNG built from a
    /// gradient, checked in under `tests/fixtures/raw/synthetic-bayer-128.dng`
    /// (see `tests/fixtures/raw/licenses.md`).
    #[test]
    fn synthetic_dng_matches_golden() {
        use crate::color::delta_e::delta_e_stats;

        let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/raw");
        let raw_path = fixture_dir.join("synthetic-bayer-128.dng");
        let golden_path = fixture_dir.join("synthetic-bayer-128.golden.png");

        let img = load_image(
            &raw_path,
            color::srgb_icc_bytes(),
            false,
            RawPipelineFlags::default(),
            1.0,
        )
        .expect("synthetic DNG should decode");
        assert_eq!(
            (img.width, img.height),
            (128, 128),
            "synthetic DNG dimensions drifted"
        );

        let rgba_bytes: &[u8] = match &img.pixels {
            super::PixelBuffer::Rgba8(v) => v.as_slice(),
            super::PixelBuffer::Rgba16F(_) => {
                panic!(
                    "synthetic DNG shouldn't be HDR: hdr_output is off-by-default via default_flags path when no EDR display is available; this fixture test runs SDR-only"
                )
            }
        };

        if std::env::var("PRVW_UPDATE_GOLDENS").ok().as_deref() == Some("1") {
            // RGBA8 -> RGB8 for PNG.
            let mut rgb: Vec<u8> = Vec::with_capacity((img.width * img.height * 3) as usize);
            for chunk in rgba_bytes.chunks_exact(4) {
                rgb.extend_from_slice(&chunk[..3]);
            }
            let buf = image::ImageBuffer::<image::Rgb<u8>, _>::from_raw(img.width, img.height, rgb)
                .expect("RGB buffer size mismatch");
            buf.save(&golden_path).expect("couldn't write golden");
            println!("Updated golden: {}", golden_path.display());
            return;
        }

        let golden = image::open(&golden_path)
            .unwrap_or_else(|e| {
                panic!(
                    "couldn't read golden PNG at {}: {e}. \
                     Run `PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden` to create it.",
                    golden_path.display()
                )
            })
            .to_rgb8();
        assert_eq!(
            (golden.width(), golden.height()),
            (img.width, img.height),
            "golden PNG dimensions don't match decoded output"
        );

        // Promote both to RGBA8 so `delta_e_stats` can diff them.
        let actual_rgba = rgba_bytes.to_vec();
        let mut golden_rgba: Vec<u8> =
            Vec::with_capacity((golden.width() * golden.height() * 4) as usize);
        for chunk in golden.as_raw().chunks_exact(3) {
            golden_rgba.extend_from_slice(chunk);
            golden_rgba.push(255);
        }

        let stats = delta_e_stats(&golden_rgba, &actual_rgba);
        // Tolerances: mean < 0.5 catches any gross pipeline drift; max < 3.0
        // tolerates a handful of border pixels that may round differently
        // across macOS versions. Tighten as needed if Phase 2+ introduces
        // deterministic pipelines we want to lock down harder.
        assert!(
            stats.mean < 0.5,
            "mean Delta-E {} exceeds 0.5 (max {}, p95 {}). \
             Run `PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden` if this change was intentional.",
            stats.mean,
            stats.max,
            stats.p95
        );
        assert!(
            stats.max < 3.0,
            "max Delta-E {} exceeds 3.0 (mean {}, p95 {}). \
             Run `PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden` if this change was intentional.",
            stats.max,
            stats.mean,
            stats.p95
        );
    }
}
