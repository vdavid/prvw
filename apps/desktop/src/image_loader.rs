use std::path::Path;

/// Decoded image data ready for GPU upload.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
}

/// Decode an image file to RGBA8 pixel data.
/// Supports JPEG, PNG, GIF (first frame), WebP, BMP, and TIFF.
pub fn load_image(path: &Path) -> Result<DecodedImage, String> {
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
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif"
    )
}
