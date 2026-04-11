use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Decoded image data ready for GPU upload.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
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

/// Read a file in 64 KB chunks, checking a cancellation flag between chunks.
fn read_file_cancellable(path: &Path, cancelled: &AtomicBool) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
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

/// JPEG extensions eligible for the fast zune-jpeg decode path.
fn is_jpeg_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "jpe" | "jfif"
    )
}

/// Decode an image file to RGBA8 pixel data.
/// JPEGs use zune-jpeg (SIMD-accelerated). Everything else goes through the `image` crate.
pub fn load_image(path: &Path) -> Result<DecodedImage, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    log::debug!("Loading {}", path.display());
    let start = Instant::now();

    let result = if is_jpeg_extension(ext) {
        load_jpeg(path)
    } else {
        load_generic(path)
    };

    match &result {
        Ok(image) => {
            let duration = start.elapsed();
            let decoded_size = format_decoded_size(image.rgba_data.len());
            let format_name = if is_jpeg_extension(ext) {
                "JPEG via zune-jpeg".to_string()
            } else {
                ext.to_uppercase()
            };
            log::info!(
                "Decoded {format_name}: {}x{} ({decoded_size}) in {}ms",
                image.width,
                image.height,
                duration.as_millis()
            );
        }
        Err(msg) => {
            log::warn!("Decode failed for {}: {msg}", path.display());
        }
    }

    result
}

/// Decode an image file to RGBA8 pixel data, with cancellation support.
/// JPEGs use zune-jpeg (SIMD-accelerated). Everything else goes through the `image` crate.
/// Returns `Err("cancelled")` if the cancellation flag is set during the read or before decoding.
pub fn load_image_cancellable(
    path: &Path,
    cancelled: &AtomicBool,
) -> Result<DecodedImage, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    log::debug!("Loading (cancellable) {}", path.display());
    let start = Instant::now();

    let result = if is_jpeg_extension(ext) {
        load_jpeg_cancellable(path, cancelled)
    } else {
        load_generic_cancellable(path, cancelled)
    };

    match &result {
        Ok(image) => {
            let duration = start.elapsed();
            let decoded_size = format_decoded_size(image.rgba_data.len());
            let format_name = if is_jpeg_extension(ext) {
                "JPEG via zune-jpeg".to_string()
            } else {
                ext.to_uppercase()
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

    result
}

/// Fast JPEG decode via zune-jpeg with SIMD options.
fn load_jpeg(path: &Path) -> Result<DecodedImage, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    decode_jpeg(path, bytes)
}

/// Fast JPEG decode with cancellable file read.
fn load_jpeg_cancellable(
    path: &Path,
    cancelled: &AtomicBool,
) -> Result<DecodedImage, String> {
    let bytes = read_file_cancellable(path, cancelled)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    decode_jpeg(path, bytes)
}

/// Decode JPEG bytes (shared by cancellable and non-cancellable paths).
fn decode_jpeg(path: &Path, bytes: Vec<u8>) -> Result<DecodedImage, String> {
    let options = zune_core::options::DecoderOptions::new_fast();
    let cursor = std::io::Cursor::new(bytes);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(cursor, options);

    let rgb = decoder
        .decode()
        .map_err(|e| format!("Couldn't decode JPEG {}: {e}", path.display()))?;

    let info = decoder
        .info()
        .ok_or_else(|| format!("No image info for {}", path.display()))?;

    let width = info.width as u32;
    let height = info.height as u32;
    let pixel_count = (width as usize) * (height as usize);

    // Convert RGB -> RGBA (add alpha = 255)
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for chunk in rgb.chunks_exact(3) {
        rgba.push(chunk[0]);
        rgba.push(chunk[1]);
        rgba.push(chunk[2]);
        rgba.push(255);
    }

    Ok(DecodedImage {
        width,
        height,
        rgba_data: rgba,
    })
}

/// Fallback: decode via the `image` crate (PNG, WebP, GIF, BMP, TIFF, etc.).
fn load_generic(path: &Path) -> Result<DecodedImage, String> {
    let img = image::open(path).map_err(|e| format!("Couldn't open {}: {e}", path.display()))?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(DecodedImage {
        width,
        height,
        rgba_data: rgba.into_raw(),
    })
}

/// Fallback decode with cancellable file read.
fn load_generic_cancellable(
    path: &Path,
    cancelled: &AtomicBool,
) -> Result<DecodedImage, String> {
    let bytes = read_file_cancellable(path, cancelled)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Couldn't decode {}: {e}", path.display()))?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(DecodedImage {
        width,
        height,
        rgba_data: rgba.into_raw(),
    })
}

/// Check if a file extension is a supported image format.
pub fn is_supported_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif"
    )
}
