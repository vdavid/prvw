//! JPEG decoding via `zune-jpeg` (SIMD-accelerated).
//!
//! `zune-jpeg` significantly outperforms the `image` crate's JPEG path on Apple
//! Silicon, so we use it unconditionally for JPEGs. See the Cargo.toml
//! `[profile.dev.package.zune-jpeg]` override — debug builds are unusably slow
//! without `opt-level = 3`.

use std::path::Path;

use crate::color;

use super::DecodedImage;

/// Decode JPEG bytes to RGBA8 and color-manage to the target ICC profile.
pub(super) fn decode(
    path: &Path,
    bytes: Vec<u8>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
) -> Result<DecodedImage, String> {
    // Decode straight to RGBA: zune fills the alpha (255) inline during its
    // SIMD color-convert, so we skip a separate full-image RGB→RGBA pass (and
    // the second framebuffer it needed). zune also handles CMYK / grayscale →
    // RGBA, which a hand-rolled 3-channel repack would mishandle.
    let options = zune_core::options::DecoderOptions::new_fast()
        .jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGBA);
    let cursor = std::io::Cursor::new(bytes);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(cursor, options);

    let mut rgba = decoder.decode().map_err(|e| {
        format!(
            "Couldn't decode JPEG {}: {e}",
            crate::paths::for_display(path)
        )
    })?;

    let icc_profile = decoder.icc_profile();

    let info = decoder
        .info()
        .ok_or_else(|| format!("No image info for {}", crate::paths::for_display(path)))?;

    let width = info.width as u32;
    let height = info.height as u32;

    let source_icc = icc_profile
        .as_deref()
        .unwrap_or_else(|| color::srgb_icc_bytes());
    color::transform_icc(&mut rgba, source_icc, target_icc, use_relative_colorimetric);

    Ok(DecodedImage::from_rgba8(width, height, rgba))
}
