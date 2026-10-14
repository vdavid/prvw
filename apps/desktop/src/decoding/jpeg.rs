//! JPEG decoding via `zune-jpeg` (SIMD-accelerated).
//!
//! `zune-jpeg` significantly outperforms the `image` crate's JPEG path on Apple
//! Silicon, so we use it unconditionally for JPEGs. See the Cargo.toml
//! `[profile.dev.package.zune-jpeg]` override — debug builds are unusably slow
//! without `opt-level = 3`.
//!
//! With HDR active, a JPEG carrying a gain map comes back `GainMapped`: its base
//! in Display P3 and the map beside it, for the shader to apply
//! (`super::gain_map`). Everything else, and every JPEG with HDR off, takes the
//! SDR path: color-managed into the display's profile.

use std::path::Path;
use std::time::Instant;

use crate::color;
use crate::color::hdr::gain_map::GainMapSpaces;

use super::DecodedImage;
use super::gain_map::{self, GainMapPayload, PrimaryHeaders};

/// Decode JPEG bytes to RGBA8 and color-manage to the target ICC profile.
/// `hdr_active` (HDR on and a display that can show it) lets a gain-map JPEG
/// keep its map; off, the map is never looked for.
pub(super) fn decode(
    path: &Path,
    bytes: Vec<u8>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    hdr_active: bool,
) -> Result<DecodedImage, String> {
    let start = Instant::now();
    // Decode straight to RGBA: zune fills the alpha (255) inline during its
    // SIMD color-convert, so we skip a separate full-image RGB→RGBA pass (and
    // the second framebuffer it needed). zune also handles CMYK / grayscale →
    // RGBA, which a hand-rolled 3-channel repack would mishandle.
    let options = zune_core::options::DecoderOptions::new_fast()
        .jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGBA);
    let cursor = std::io::Cursor::new(bytes.as_slice());
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

    if hdr_active {
        let primary = PrimaryHeaders {
            xmp: decoder.xmp().map(Vec::as_slice),
            exif: decoder.exif().map(Vec::as_slice),
            mpf: info
                .multi_picture_information
                .as_deref()
                .zip(info.multi_picture_information_offset)
                .and_then(|(index, offset)| Some((index, usize::try_from(offset).ok()?))),
            iso: info
                .gain_map_info
                .iter()
                .map(|g| g.data.as_slice())
                .collect(),
        };
        let base_ms = start.elapsed().as_secs_f64() * 1e3;
        if let Some(found) = gain_map::locate(&bytes, &primary) {
            let located_ms = start.elapsed().as_secs_f64() * 1e3;
            match decode_map(&bytes[found.map.clone()], width, height) {
                Ok((map_width, map_height, map, channels)) => {
                    let map_ms = start.elapsed().as_secs_f64() * 1e3;
                    let mut params = found.params;
                    // A single-channel map applies channel 0's parameters to all three, as
                    // `ultrahdr-core` does; the map's RGB replicas would otherwise each read
                    // their own.
                    if channels == 1 {
                        params.channels = [params.channels[0]; 3];
                    }
                    let spaces = GainMapSpaces::new(
                        source_icc,
                        found.map_icc.as_deref(),
                        params.use_base_color_space,
                    );
                    color::transform_icc(
                        &mut rgba,
                        source_icc,
                        color::display_p3_icc_bytes(),
                        use_relative_colorimetric,
                    );
                    log::debug!(
                        "Gain-map JPEG ({:?}, {map_width}x{map_height} map, {channels} channel) \
                         [{:.1} ms]: base={base_ms:.1}, locate={:.1}, map={:.1}, icc={:.1}",
                        found.source,
                        start.elapsed().as_secs_f64() * 1e3,
                        located_ms - base_ms,
                        map_ms - located_ms,
                        start.elapsed().as_secs_f64() * 1e3 - map_ms,
                    );
                    let payload = GainMapPayload {
                        width: map_width,
                        height: map_height,
                        rgba: map,
                        params,
                        source: found.source,
                        spaces,
                        // As stored; `finalize` records the EXIF turn.
                        turn: Default::default(),
                    };
                    return Ok(DecodedImage::from_gain_mapped(width, height, rgba, payload));
                }
                Err(e) => log::debug!(
                    "Gain map in {} didn't decode, showing it as SDR: {e}",
                    crate::paths::for_display(path)
                ),
            }
        }
    }

    color::transform_icc(&mut rgba, source_icc, target_icc, use_relative_colorimetric);

    Ok(DecodedImage::from_rgba8(width, height, rgba))
}

/// Decode the gain-map image to RGBA8 (a grayscale map replicated into R, G,
/// and B), with no color management: its values are data. Returns its size
/// and channel count. A map larger than its base on either side is refused,
/// which also bounds what a hostile file can make us allocate.
fn decode_map(
    bytes: &[u8],
    base_width: u32,
    base_height: u32,
) -> Result<(u32, u32, Vec<u8>, u8), String> {
    let options = zune_core::options::DecoderOptions::new_fast()
        .jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGBA);
    let mut decoder =
        zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(bytes), options);
    decoder.decode_headers().map_err(|e| e.to_string())?;
    let info = decoder.info().ok_or("no map info")?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    if width == 0 || height == 0 || width > base_width || height > base_height {
        return Err(format!(
            "a {width}x{height} map for a {base_width}x{base_height} base"
        ));
    }
    let channels = if info.components == 1 { 1 } else { 3 };
    let rgba = decoder.decode().map_err(|e| e.to_string())?;
    if rgba.len() != (width * height * 4) as usize {
        return Err(format!("map decoded to {} bytes", rgba.len()));
    }
    Ok((width, height, rgba, channels))
}
