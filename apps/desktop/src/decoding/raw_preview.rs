//! Fast "quick preview" for RAW files.
//!
//! Pulls the camera's embedded JPEG preview (via rawler) and returns a
//! deliberately downscaled, orientation-corrected, color-managed RGBA8 image.
//! Shown as a soft placeholder the instant a RAW navigation misses the cache,
//! while the full develop (~450 ms on a 20 MP RAW) runs in the background.
//!
//! **No RAW develop happens here** — just an embedded-JPEG decode (tens of ms),
//! so it's cheap enough to run before the real decode and still feel instant.
//!
//! **Deliberately downscaled.** The placeholder should read as "still loading",
//! so the sharp full render is a crisp *upgrade*, never a confusing change — the
//! camera's JPEG look (tone, color) differs from our develop, and we don't want
//! a too-good preview that the user then "loses" when our version lands. Keeping
//! it soft (like the QuickLook-thumb placeholder) hides that mismatch.

use std::path::Path;

use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

use crate::color;

use super::DecodedImage;
use super::orientation::apply_orientation_bytes;

/// Long-edge size the embedded preview is downscaled to. Small on purpose so
/// the placeholder looks soft. **Tunable**: raise for a sharper preview, lower
/// for blurrier. 1024 matches the QuickLook-thumb bucket the app already uses.
const PREVIEW_LONG_EDGE_PX: u32 = 1024;

/// Extract and prepare the embedded preview for `path`, or `None` if the file
/// has no embedded preview / rawler can't read it. Never runs the RAW develop.
pub(super) fn decode_raw_preview(
    path: &Path,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
) -> Option<DecodedImage> {
    let start = std::time::Instant::now();
    let src = RawSource::new(path).ok()?;
    let decoder = rawler::get_decoder(&src).ok()?;
    let params = RawDecodeParams::default();

    // Embedded preview if present, else the embedded full-size image. Both are
    // stored JPEGs in the RAW container — cheap. Explicitly NOT the develop.
    let dynimg = decoder
        .preview_image(&src, &params)
        .ok()
        .flatten()
        .or_else(|| decoder.full_image(&src, &params).ok().flatten())?;

    // rawler hard-codes `RawImage.orientation` to Normal; the real EXIF value
    // is on the decoder metadata (same source the full develop uses). The
    // embedded preview is stored un-oriented, so apply it or portrait shots
    // would flash sideways before snapping upright.
    let orientation = decoder
        .raw_metadata(&src, &params)
        .ok()
        .and_then(|m| m.exif.orientation)
        .unwrap_or(1);

    // Downscale to the soft placeholder size. `thumbnail` preserves aspect,
    // only ever downscales (never upscales a small preview), and uses a fast
    // box filter — slightly softer than a Lanczos resize, which is exactly what
    // we want for a "not final" placeholder.
    let scaled = dynimg.thumbnail(PREVIEW_LONG_EDGE_PX, PREVIEW_LONG_EDGE_PX);

    let rgba = scaled.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut data = rgba.into_raw();

    // Camera previews are sRGB-ish; color-manage to the display like the JPEG
    // path so the placeholder isn't wildly off. Exactness is moot — it's soft
    // and transient.
    color::transform_icc(
        &mut data,
        color::srgb_icc_bytes(),
        target_icc,
        use_relative_colorimetric,
    );

    let (ow, oh) = apply_orientation_bytes(w, h, &mut data, orientation, 4);
    log::debug!(
        "RAW preview {} ({ow}x{oh}, orient {orientation}) in {}ms",
        path.display(),
        start.elapsed().as_millis()
    );
    Some(DecodedImage::from_rgba8(ow, oh, data))
}
