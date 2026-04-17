//! Generic decoding via the `image` crate (PNG, GIF, WebP, BMP, TIFF).
//!
//! ## ICC extraction ordering (subtle)
//!
//! `ImageReader::into_decoder()` returns `impl ImageDecoder`. `icc_profile()` takes
//! `&mut self`, and `DynamicImage::from_decoder()` consumes the decoder. So you
//! must call `icc_profile()` first, then `from_decoder()`. Reversing won't compile.

use std::io::Cursor;
use std::path::Path;

use image::ImageDecoder;

use crate::color;

use super::DecodedImage;

/// Decode a non-JPEG image via the `image` crate and color-manage to the target ICC profile.
pub(super) fn decode(
    path: &Path,
    bytes: Vec<u8>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
) -> Result<DecodedImage, String> {
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
    color::transform_icc(
        &mut rgba_data,
        source_icc,
        target_icc,
        use_relative_colorimetric,
    );

    Ok(DecodedImage {
        width,
        height,
        rgba_data,
    })
}
