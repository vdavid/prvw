//! What Copy and Print both need from a file on Windows.
//!
//! The counterpart of `platform::macos::ui_common`, and it makes the same point: neither the
//! clipboard nor a printer wants the buffer `App` already holds. That one is transformed to the
//! display's profile and may be half-float HDR, so handing it over would shift colours in
//! whatever opens or prints it next. Both features re-read the original file instead.
//!
//! Where macOS re-reads through ImageIO, Windows re-reads through Prvw's own decoder — so unlike
//! macOS, a copied or printed RAW matches what the viewer showed.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::color;
use crate::decoding::{self, PixelBuffer, RawPipelineFlags};

/// Decode the original file to 8-bit sRGB: the only space a DIB, and a GDI printer DC, have any
/// way of saying anything in.
///
/// `edr_headroom` is pinned to 1.0 (SDR), which is what keeps the RAW path on its 8-bit output.
/// `raw_flags` and `relative_colorimetric` are the app's current decode settings, so a RAW comes
/// out the way the viewer renders it.
pub(crate) fn decode_srgb(
    path: &Path,
    raw_flags: RawPipelineFlags,
    relative_colorimetric: bool,
) -> Option<(u32, u32, Vec<u8>)> {
    let decoded = decoding::load_image(
        path,
        &AtomicBool::new(false),
        color::srgb_icc_bytes(),
        relative_colorimetric,
        raw_flags,
        1.0,
        None,
        None,
    )
    .ok()?;
    match decoded.pixels {
        PixelBuffer::Rgba8(rgba) => Some((decoded.width, decoded.height, rgba)),
        // Unreachable with an SDR headroom, and worth a line rather than a silent skip if the
        // decoder's contract ever changes.
        PixelBuffer::Rgba16F(_) => {
            log::warn!(
                "Got a half-float buffer, which neither the clipboard nor a printer can carry"
            );
            None
        }
    }
}
