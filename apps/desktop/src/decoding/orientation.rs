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

/// Apply EXIF orientation transform to an RGBA pixel buffer.
/// Returns the new (width, height) after rotation.
pub(super) fn apply_orientation(
    width: u32,
    height: u32,
    rgba: &mut Vec<u8>,
    orientation: u16,
) -> (u32, u32) {
    // Zero-dimension buffers have nothing to rotate and would underflow the
    // index math below (orientation 2's `(width - 1) * 4`, etc.).
    if width == 0 || height == 0 {
        return (width, height);
    }
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
