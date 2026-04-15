use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use image::ImageDecoder;
use nom_exif::{EntryValue, ExifTag, MediaParser, MediaSource};

use crate::color;

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

/// Parse EXIF orientation from raw file bytes. Returns 1 (normal) on any failure.
fn parse_exif_orientation(bytes: &[u8], filename: &str) -> u16 {
    let orientation = (|| -> Option<u16> {
        let mut parser = MediaParser::new();
        let cursor = Cursor::new(bytes);
        let ms = MediaSource::seekable(cursor).ok()?;
        if !ms.has_exif() {
            return None;
        }
        let iter: nom_exif::ExifIter = parser.parse(ms).ok()?;
        let exif: nom_exif::Exif = iter.into();
        let value = exif.get(ExifTag::Orientation)?;
        match value {
            EntryValue::U16(v) => Some(*v),
            EntryValue::U32(v) => Some(*v as u16),
            EntryValue::U8(v) => Some(*v as u16),
            _ => None,
        }
    })()
    .unwrap_or(1);

    if orientation != 1 {
        log::debug!("EXIF orientation: {orientation} for {filename}");
    }
    orientation
}

/// Apply EXIF orientation transform to an RGBA pixel buffer.
/// Returns the new (width, height) after rotation.
fn apply_orientation(width: u32, height: u32, rgba: &mut Vec<u8>, orientation: u16) -> (u32, u32) {
    match orientation {
        1 => (width, height),
        2 => {
            // Flip horizontal: reverse each row
            let stride = (width as usize) * 4;
            for row in rgba.chunks_exact_mut(stride) {
                let mut left = 0usize;
                let mut right = (width as usize - 1) * 4;
                while left < right {
                    for i in 0..4 {
                        row.swap(left + i, right + i);
                    }
                    left += 4;
                    right -= 4;
                }
            }
            (width, height)
        }
        3 => {
            // Rotate 180: reverse the entire pixel array
            let pixel_count = (width as usize) * (height as usize);
            for i in 0..pixel_count / 2 {
                let j = pixel_count - 1 - i;
                let (a, b) = (i * 4, j * 4);
                for k in 0..4 {
                    rgba.swap(a + k, b + k);
                }
            }
            (width, height)
        }
        4 => {
            // Flip vertical: reverse row order
            let stride = (width as usize) * 4;
            let h = height as usize;
            for row_idx in 0..h / 2 {
                let opposite = h - 1 - row_idx;
                let (top, bottom) = (row_idx * stride, opposite * stride);
                for col in 0..stride {
                    rgba.swap(top + col, bottom + col);
                }
            }
            (width, height)
        }
        5 => {
            // Transpose (swap x/y)
            let (w, h) = (width as usize, height as usize);
            let mut out = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 4;
                    let dst = (x * h + y) * 4;
                    out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
                }
            }
            *rgba = out;
            (height, width)
        }
        6 => {
            // Rotate 90 CW: new[x][h-1-y] = old[y][x]
            let (w, h) = (width as usize, height as usize);
            let mut out = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 4;
                    let dst = (x * h + (h - 1 - y)) * 4;
                    out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
                }
            }
            *rgba = out;
            (height, width)
        }
        7 => {
            // Transverse: rotate 90 CW + flip horizontal
            // new[w-1-x][h-1-y] = old[y][x]  => new dims are (h, w)
            let (w, h) = (width as usize, height as usize);
            let mut out = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 4;
                    let dst = ((w - 1 - x) * h + (h - 1 - y)) * 4;
                    out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
                }
            }
            *rgba = out;
            (height, width)
        }
        8 => {
            // Rotate 270 CW (= 90 CCW): new[w-1-x][y] = old[y][x]
            let (w, h) = (width as usize, height as usize);
            let mut out = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 4;
                    let dst = ((w - 1 - x) * h + y) * 4;
                    out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
                }
            }
            *rgba = out;
            (height, width)
        }
        _ => {
            log::warn!("Unknown EXIF orientation value: {orientation}, ignoring");
            (width, height)
        }
    }
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

/// JPEG extensions eligible for the fast zune-jpeg decode path.
fn is_jpeg_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "jpe" | "jfif"
    )
}

/// Decode an image file to RGBA8 pixel data, color-managed to the given target ICC profile.
/// JPEGs use zune-jpeg (SIMD-accelerated). Everything else goes through the `image` crate.
/// Applies EXIF orientation correction automatically.
/// Images without an embedded ICC profile are assumed sRGB and transformed to `target_icc`.
pub fn load_image(path: &Path, target_icc: &[u8]) -> Result<DecodedImage, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

    log::debug!("Loading {}", path.display());
    let start = Instant::now();

    let bytes =
        std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let orientation = parse_exif_orientation(&bytes, filename);

    let result = if is_jpeg_extension(ext) {
        decode_jpeg(path, bytes, target_icc)
    } else {
        decode_generic(path, bytes, target_icc)
    };

    let result = result.map(|mut img| {
        let (old_w, old_h) = (img.width, img.height);
        let (new_w, new_h) =
            apply_orientation(img.width, img.height, &mut img.rgba_data, orientation);
        if (new_w, new_h) != (old_w, old_h) {
            log::debug!(
                "Applied rotation: orientation {orientation} ({old_w}x{old_h} -> {new_w}x{new_h})"
            );
        }
        img.width = new_w;
        img.height = new_h;
        img
    });

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
/// Applies EXIF orientation correction automatically.
/// Returns `Err("cancelled")` if the cancellation flag is set during the read or before decoding.
pub fn load_image_cancellable(
    path: &Path,
    cancelled: &AtomicBool,
    target_icc: &[u8],
) -> Result<DecodedImage, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

    log::debug!("Loading (cancellable) {}", path.display());
    let start = Instant::now();

    let bytes = read_file_cancellable(path, cancelled)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }

    let orientation = parse_exif_orientation(&bytes, filename);

    let result = if is_jpeg_extension(ext) {
        decode_jpeg(path, bytes, target_icc)
    } else {
        decode_generic(path, bytes, target_icc)
    };

    let result = result.map(|mut img| {
        let (old_w, old_h) = (img.width, img.height);
        let (new_w, new_h) =
            apply_orientation(img.width, img.height, &mut img.rgba_data, orientation);
        if (new_w, new_h) != (old_w, old_h) {
            log::debug!(
                "Applied rotation: orientation {orientation} ({old_w}x{old_h} -> {new_w}x{new_h})"
            );
        }
        img.width = new_w;
        img.height = new_h;
        img
    });

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

/// Decode JPEG bytes (shared by cancellable and non-cancellable paths).
fn decode_jpeg(path: &Path, bytes: Vec<u8>, target_icc: &[u8]) -> Result<DecodedImage, String> {
    let options = zune_core::options::DecoderOptions::new_fast();
    let cursor = std::io::Cursor::new(bytes);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(cursor, options);

    let rgb = decoder
        .decode()
        .map_err(|e| format!("Couldn't decode JPEG {}: {e}", path.display()))?;

    let icc_profile = decoder.icc_profile();

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

    let source_icc = icc_profile
        .as_deref()
        .unwrap_or_else(|| color::srgb_icc_bytes());
    color::transform_icc(&mut rgba, source_icc, target_icc);

    Ok(DecodedImage {
        width,
        height,
        rgba_data: rgba,
    })
}

/// Fallback: decode via the `image` crate (PNG, WebP, GIF, BMP, TIFF, etc.).
fn decode_generic(path: &Path, bytes: Vec<u8>, target_icc: &[u8]) -> Result<DecodedImage, String> {
    let cursor = Cursor::new(&bytes);
    let reader = image::ImageReader::new(cursor)
        .with_guessed_format()
        .map_err(|e| format!("Couldn't identify format for {}: {e}", path.display()))?;

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Couldn't decode {}: {e}", path.display()))?;

    let icc_profile = decoder.icc_profile().ok().flatten();

    let img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Couldn't decode {}: {e}", path.display()))?;

    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut rgba_data = rgba.into_raw();

    let source_icc = icc_profile
        .as_deref()
        .unwrap_or_else(|| color::srgb_icc_bytes());
    color::transform_icc(&mut rgba_data, source_icc, target_icc);

    Ok(DecodedImage {
        width,
        height,
        rgba_data,
    })
}

/// Check if a file extension is a supported image format.
pub fn is_supported_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif"
    )
}
