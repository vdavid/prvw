//! Camera RAW decoding via the `rawler` crate.
//!
//! Covers the 10 formats listed in [`super::dispatch::is_raw_extension`]. Rawler
//! exposes a two-stage pipeline: `raw_image` extracts the sensor mosaic plus the
//! per-camera metadata, then `RawDevelop` runs demosaic, white balance, color
//! matrix, and sRGB gamma in a single pass (parallelised via rayon). The
//! resulting RGB16 image is color-managed to the target ICC profile.
//!
//! ## Orientation quirk
//!
//! `RawImage.orientation` is hard-coded to `Normal` in rawler; the real EXIF
//! orientation lives on `raw_metadata(...).exif.orientation`. We propagate the
//! latter through the shared `apply_orientation` helper in `orientation.rs`.
//!
//! ## Fujifilm X-Trans
//!
//! RAF files use bilinear demosaic only (no Markesteijn). Output is usable but
//! less detailed than a dedicated X-Trans algorithm. Fine for a viewer.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::RawDevelop;
use rawler::rawsource::RawSource;

use crate::color;

use super::DecodedImage;

/// Decode a RAW file via rawler's develop pipeline and color-manage to `target_icc`.
/// Returns the developed RGBA8 buffer plus the EXIF orientation read from rawler's
/// metadata (rawler's own `RawImage.orientation` is always `Normal`, so the caller
/// can't trust that one).
pub(super) fn decode(
    path: &Path,
    bytes: Vec<u8>,
    cancelled: Option<&AtomicBool>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
) -> Result<(DecodedImage, u16), String> {
    check_cancelled(cancelled)?;

    // `new_from_shared_vec` hands ownership over without copying; `new_from_slice`
    // would duplicate the buffer, which hurts on a 40 MB sensor file.
    let src = RawSource::new_from_shared_vec(Arc::new(bytes)).with_path(path);

    check_cancelled(cancelled)?;

    let decoder = rawler::get_decoder(&src)
        .map_err(|e| format!("Couldn't open RAW {}: {e}", path.display()))?;

    check_cancelled(cancelled)?;

    let params = RawDecodeParams::default();
    let raw = decoder
        .raw_image(&src, &params, false)
        .map_err(|e| format!("Couldn't decode RAW {}: {e}", path.display()))?;

    check_cancelled(cancelled)?;

    // The develop pipeline is multi-threaded internally; cancellation between
    // stages is as fine-grained as we can get without forking rawler.
    let intermediate = RawDevelop::default()
        .develop_intermediate(&raw)
        .map_err(|e| format!("Couldn't develop RAW {}: {e}", path.display()))?;

    let dyn_img = intermediate
        .to_dynamic_image()
        .ok_or_else(|| format!("Develop produced no image for {}", path.display()))?;

    let rgb = dyn_img.to_rgb8();
    let (width, height) = (rgb.width(), rgb.height());
    let rgb_data = rgb.into_raw();

    let pixel_count = (width as usize) * (height as usize);
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for chunk in rgb_data.chunks_exact(3) {
        rgba.push(chunk[0]);
        rgba.push(chunk[1]);
        rgba.push(chunk[2]);
        rgba.push(255);
    }

    let source_icc = color::srgb_icc_bytes();
    color::transform_icc(&mut rgba, source_icc, target_icc, use_relative_colorimetric);

    let orientation = decoder
        .raw_metadata(&src, &params)
        .ok()
        .and_then(|meta| meta.exif.orientation)
        .unwrap_or(1);

    Ok((
        DecodedImage {
            width,
            height,
            rgba_data: rgba,
        },
        orientation,
    ))
}

fn check_cancelled(cancelled: Option<&AtomicBool>) -> Result<(), String> {
    if let Some(flag) = cancelled
        && flag.load(Ordering::Relaxed)
    {
        return Err("cancelled".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_bytes_return_error() {
        let bytes = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03];
        let result = decode(
            Path::new("bogus.arw"),
            bytes,
            None,
            color::srgb_icc_bytes(),
            false,
        );
        assert!(result.is_err(), "expected error for malformed bytes");
    }

    #[test]
    fn cancellation_short_circuits() {
        let flag = AtomicBool::new(true);
        let result = decode(
            Path::new("anything.arw"),
            vec![0u8; 32],
            Some(&flag),
            color::srgb_icc_bytes(),
            false,
        );
        assert_eq!(result.err().as_deref(), Some("cancelled"));
    }

    /// Decode the local ARW fixture if it exists. Gated behind `#[ignore]`
    /// because the fixture lives outside the repo. Run with
    /// `cargo test decoding::raw::tests::arw_fixture_decodes -- --ignored`.
    #[test]
    #[ignore]
    fn arw_fixture_decodes() {
        let path = Path::new("/tmp/raw/sample1.arw");
        let bytes = std::fs::read(path).expect("fixture missing");
        let (img, orientation) =
            decode(path, bytes, None, color::srgb_icc_bytes(), false).expect("decode failed");
        assert_eq!(orientation, 1);
        assert_eq!((img.width, img.height), (5456, 3632));
        assert_eq!(img.rgba_data.len(), 5456 * 3632 * 4);
    }

    #[test]
    #[ignore]
    fn dng_fixture_decodes() {
        let path = Path::new("/tmp/raw/sample2.dng");
        let bytes = std::fs::read(path).expect("fixture missing");
        let (img, orientation) =
            decode(path, bytes, None, color::srgb_icc_bytes(), false).expect("decode failed");
        // Pre-orientation dimensions match the POC: 3990x3000 sideways.
        assert_eq!((img.width, img.height), (3990, 3000));
        assert!(matches!(orientation, 6 | 8));
        assert_eq!(img.rgba_data.len(), 3990 * 3000 * 4);
    }
}
