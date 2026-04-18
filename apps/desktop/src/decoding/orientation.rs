//! EXIF orientation parsing and pixel-buffer rotation.
//!
//! Backends decode into the camera's native pixel layout. EXIF tag 0x0112
//! (Orientation) tells us how to rotate/flip the buffer so the image looks right
//! when displayed. Values 1–8 per the EXIF spec; anything else is treated as 1.

use std::io::Cursor;

use nom_exif::{EntryValue, ExifTag, MediaParser, MediaSource};

/// Parse EXIF orientation from raw file bytes. Returns 1 (normal) on any failure.
pub(super) fn parse_exif_orientation(bytes: &[u8], filename: &str) -> u16 {
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

/// Apply EXIF orientation transform to an RGBA byte buffer with 4 bytes
/// per pixel. Returns the new (width, height) after rotation. Same logic as
/// before Phase 5; kept as a byte-stride specialisation because it's the
/// hot path for every non-RAW format.
pub(super) fn apply_orientation_bytes(
    width: u32,
    height: u32,
    rgba: &mut Vec<u8>,
    orientation: u16,
    bpp: usize,
) -> (u32, u32) {
    apply_orientation_generic(width, height, rgba, orientation, bpp, 0u8)
}

/// RGBA16F variant: each "pixel" is four `u16`s (R, G, B, A half-floats).
/// Same rotation logic as the byte path; we just swap 4-element blocks of
/// `u16` instead of `bpp`-element blocks of `u8`.
pub(super) fn apply_orientation_u16(
    width: u32,
    height: u32,
    rgba: &mut Vec<u16>,
    orientation: u16,
    channels_per_pixel: usize,
) -> (u32, u32) {
    apply_orientation_generic(width, height, rgba, orientation, channels_per_pixel, 0u16)
}

/// Generic orientation fixup: walks `stride`-sized pixel blocks and swaps /
/// re-layouts them per EXIF orientation. `T` is the channel unit (u8 for
/// RGBA8, u16 for RGBA16F). `_zero` disambiguates the type for `vec![_; n]`
/// at the allocation sites.
fn apply_orientation_generic<T: Copy + Default>(
    width: u32,
    height: u32,
    pixels: &mut Vec<T>,
    orientation: u16,
    block: usize,
    _zero: T,
) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (width, height);
    }
    match orientation {
        1 => (width, height),
        2 => {
            let stride = (width as usize) * block;
            for row in pixels.chunks_exact_mut(stride) {
                let mut left = 0usize;
                let mut right = (width as usize - 1) * block;
                while left < right {
                    for i in 0..block {
                        row.swap(left + i, right + i);
                    }
                    left += block;
                    right -= block;
                }
            }
            (width, height)
        }
        3 => {
            let pixel_count = (width as usize) * (height as usize);
            for i in 0..pixel_count / 2 {
                let j = pixel_count - 1 - i;
                let (a, b) = (i * block, j * block);
                for k in 0..block {
                    pixels.swap(a + k, b + k);
                }
            }
            (width, height)
        }
        4 => {
            let stride = (width as usize) * block;
            let h = height as usize;
            for row_idx in 0..h / 2 {
                let opposite = h - 1 - row_idx;
                let (top, bottom) = (row_idx * stride, opposite * stride);
                for col in 0..stride {
                    pixels.swap(top + col, bottom + col);
                }
            }
            (width, height)
        }
        5 => {
            let (w, h) = (width as usize, height as usize);
            let mut out: Vec<T> = vec![T::default(); w * h * block];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * block;
                    let dst = (x * h + y) * block;
                    out[dst..dst + block].copy_from_slice(&pixels[src..src + block]);
                }
            }
            *pixels = out;
            (height, width)
        }
        6 => {
            let (w, h) = (width as usize, height as usize);
            let mut out: Vec<T> = vec![T::default(); w * h * block];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * block;
                    let dst = (x * h + (h - 1 - y)) * block;
                    out[dst..dst + block].copy_from_slice(&pixels[src..src + block]);
                }
            }
            *pixels = out;
            (height, width)
        }
        7 => {
            let (w, h) = (width as usize, height as usize);
            let mut out: Vec<T> = vec![T::default(); w * h * block];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * block;
                    let dst = ((w - 1 - x) * h + (h - 1 - y)) * block;
                    out[dst..dst + block].copy_from_slice(&pixels[src..src + block]);
                }
            }
            *pixels = out;
            (height, width)
        }
        8 => {
            let (w, h) = (width as usize, height as usize);
            let mut out: Vec<T> = vec![T::default(); w * h * block];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * block;
                    let dst = ((w - 1 - x) * h + y) * block;
                    out[dst..dst + block].copy_from_slice(&pixels[src..src + block]);
                }
            }
            *pixels = out;
            (height, width)
        }
        _ => {
            log::warn!("Unknown EXIF orientation value: {orientation}, ignoring");
            (width, height)
        }
    }
}
