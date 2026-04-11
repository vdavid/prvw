use std::path::Path;

/// Decoded image data ready for GPU upload.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
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
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if is_jpeg_extension(ext) {
        load_jpeg(path)
    } else {
        load_generic(path)
    }
}

/// Fast JPEG decode via zune-jpeg with SIMD options.
fn load_jpeg(path: &Path) -> Result<DecodedImage, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;

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

/// Check if a file extension is a supported image format.
pub fn is_supported_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif"
    )
}
