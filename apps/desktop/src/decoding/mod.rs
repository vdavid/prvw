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
//! - [`DecodedImage`] — RGBA8 pixel buffer plus dimensions, ready for GPU upload.
//! - [`load_image`] / [`load_image_cancellable`] — decode a file to `DecodedImage`,
//!   color-managed to a target ICC profile, with EXIF orientation applied.
//! - [`is_supported_extension`] — format gate used by the directory scanner.

mod dispatch;
mod generic;
mod jpeg;
mod orientation;
mod raw;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use dispatch::Backend;
use orientation::{apply_orientation, parse_exif_orientation};

/// Decoded image data ready for GPU upload.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
}

/// Decode an image file to RGBA8 pixel data, color-managed to the given target ICC profile.
/// JPEGs use zune-jpeg (SIMD-accelerated). RAW files use rawler. Everything else goes
/// through the `image` crate. Applies EXIF orientation correction automatically.
/// Images without an embedded ICC profile are assumed sRGB and transformed to `target_icc`.
pub fn load_image(
    path: &Path,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
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
    );

    log_result(&result, ext, backend, path, start);
    result
}

/// Decode an image file to RGBA8 pixel data, with cancellation support.
/// JPEGs use zune-jpeg (SIMD-accelerated). RAW files use rawler. Everything else goes
/// through the `image` crate. Applies EXIF orientation correction automatically.
/// Returns `Err("cancelled")` if the cancellation flag is set during the read or before decoding.
pub fn load_image_cancellable(
    path: &Path,
    cancelled: &AtomicBool,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
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
fn decode_with(
    backend: Backend,
    path: &Path,
    filename: &str,
    bytes: Vec<u8>,
    cancelled: Option<&AtomicBool>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
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
            )?;
            if orientation != 1 {
                log::debug!("RAW orientation: {orientation} for {filename}");
            }
            Ok(finalize(img, orientation))
        }
    }
}

/// Apply EXIF orientation and update dimensions.
fn finalize(mut img: DecodedImage, orientation: u16) -> DecodedImage {
    let (old_w, old_h) = (img.width, img.height);
    let (new_w, new_h) = apply_orientation(img.width, img.height, &mut img.rgba_data, orientation);
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
            let decoded_size = format_decoded_size(image.rgba_data.len());
            let format_name = match backend {
                Backend::Jpeg => "JPEG via zune-jpeg".to_string(),
                Backend::Raw => format!("{} via rawler", ext.to_uppercase()),
                Backend::Generic => ext.to_uppercase(),
            };
            log::info!(
                "Decoded {format_name}: {}x{} ({decoded_size}) in {}ms",
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

#[cfg(test)]
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
        let img = load_image(path, color::srgb_icc_bytes(), false).expect("decode failed");
        assert_eq!((img.width, img.height), (5456, 3632));
    }

    /// End-to-end: `load_image` on the DNG fixture. sample2 comes out of rawler
    /// as 3990x3000 but carries EXIF orientation 6 or 8, which swaps dims to
    /// 3000x3990.
    #[test]
    #[ignore]
    fn dng_end_to_end() {
        let path = Path::new("/tmp/raw/sample2.dng");
        let img = load_image(path, color::srgb_icc_bytes(), false).expect("decode failed");
        assert_eq!((img.width, img.height), (3000, 3990));
    }
}
