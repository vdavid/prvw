//! The bytes Prvw puts on the Windows clipboard.
//!
//! Copy is one action with three platform answers, and only Windows' is a byte layout:
//! macOS hands `NSPasteboard` live objects (`platform::macos::clipboard`), Windows hands the
//! system raw memory blocks it then owns, and Linux has no clipboard yet (see below). So the
//! layouts live here, pure and dependency-free, the way `scroll` holds the input mapping: a Mac
//! can test what a Windows user will paste, which is the only way this gets checked at all until
//! the app runs there. `platform::windows::clipboard` owns the `HGLOBAL`s and the Win32 calls.
//!
//! ## Which formats, and why more than one
//!
//! - **`CF_HDROP`** — the original file. Explorer pastes a copy of it, and chat apps upload it,
//!   both at full quality with the EXIF intact. It's the representation that loses nothing.
//! - **`CF_DIB`** — 24-bit bottom-up BGR, the format every Windows app has understood since
//!   1990. Editors, Office, Paint.
//! - **`CF_DIBV5`** — 32-bit BGRA, offered **only** when the image actually has transparent
//!   pixels, because that's the only case where it says anything `CF_DIB` can't.
//!
//! ## The alpha trap
//!
//! Most consumers read a 32-bit DIB's fourth byte as padding, not as alpha, so a PNG with a
//! transparent background pastes as garbage wherever the ignored byte happens to be zero. And
//! Windows *synthesises* `CF_DIB` from `CF_DIBV5` by handing over the same 32-bit pixels, so
//! offering V5 alone walks straight into it.
//!
//! Both are avoided by always writing `CF_DIB` ourselves, at 24 bits with the alpha composited
//! over white: no fourth byte to misread, and setting it explicitly means Windows synthesises
//! nothing. Consumers that do understand alpha find `CF_DIBV5` alongside it (straight, not
//! premultiplied, tagged `LCS_sRGB`). White is the composite background because a viewer's
//! canvas is not what the user is pasting into: a document is white far more often than not.
//!
//! ## Colour
//!
//! A DIB carries no ICC profile, so the pixels have to *be* sRGB. `platform::windows::clipboard`
//! decodes the original file with sRGB as the target profile for exactly that reason, and
//! `CF_DIBV5` says so in `bV5CSType`. Nothing here transforms colour; it only packs what it's
//! given.
//!
//! ## Linux
//!
//! Not built, and not this milestone's job. Copy is unreachable there anyway: Linux has no menu
//! bar (`menu::absent`) and `input::key_to_command` binds no copy key, so there is no way to
//! invoke the action. Wiring it needs an X11/Wayland selection owner that survives losing focus,
//! which is a Linux spec's decision, not a `#[cfg]` arm here. The registries record it as
//! `Missing` rather than `NotApplicable`: it applies there, it just isn't built.

use std::path::Path;

/// `BITMAPINFOHEADER`, the header `CF_DIB` starts with.
const BITMAPINFOHEADER_SIZE: u32 = 40;

/// `BITMAPV5HEADER`, the header `CF_DIBV5` starts with. Longer than V4 by the profile fields,
/// and `bV5CSType` is why it's here: it's the only one of the three that can say "sRGB".
const BITMAPV5HEADER_SIZE: u32 = 124;

/// `BI_RGB`: no compression, channels in the order the bit count implies.
const BI_RGB: u32 = 0;

/// `BI_BITFIELDS`: the channel masks in the header say where each channel sits. Required for a
/// 32-bit DIB whose fourth channel is alpha rather than padding.
const BI_BITFIELDS: u32 = 3;

/// `LCS_sRGB`, which is the ASCII `sRGB` read as a big-endian `u32`.
const LCS_SRGB: u32 = 0x7352_4742;

/// `LCS_GM_IMAGES`: the perceptual rendering intent, which is what a photo wants.
const LCS_GM_IMAGES: u32 = 4;

/// Pixels per metre, both axes: 96 DPI, Windows' own baseline. Zero would be legal and would
/// leave a consumer to guess, and the guess Word makes decides how big the pasted image lands.
const PIXELS_PER_METRE: i32 = 3780;

/// `sizeof(DROPFILES)`, and therefore the offset of the first path in a `CF_HDROP` block.
const DROPFILES_SIZE: u32 = 20;

/// The bitmap representations of one image, ready to hand to Windows.
pub struct WindowsBitmaps {
    /// `CF_DIB`: 24-bit BGR, bottom-up. Always present.
    pub dib: Vec<u8>,
    /// `CF_DIBV5`: 32-bit BGRA, bottom-up. `None` unless the image has a transparent pixel,
    /// since an opaque image's alpha channel tells a consumer nothing.
    pub dib_v5: Option<Vec<u8>>,
}

/// Pack `rgba` (8 bits per channel, straight alpha, top row first) into the DIB formats the
/// clipboard should carry for it.
///
/// `rgba` must hold `width * height * 4` bytes; a short buffer yields empty rows rather than a
/// panic, since a decoder disagreeing with its own dimensions shouldn't take the app down.
#[must_use]
pub fn windows_bitmaps(width: u32, height: u32, rgba: &[u8]) -> WindowsBitmaps {
    WindowsBitmaps {
        dib: dib_bgr24(width, height, rgba),
        dib_v5: has_transparency(rgba).then(|| dib_v5_bgra32(width, height, rgba)),
    }
}

/// Whether any pixel is less than fully opaque.
#[must_use]
pub fn has_transparency(rgba: &[u8]) -> bool {
    rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 255)
}

/// A `CF_DIB` block: `BITMAPINFOHEADER` followed by 24-bit BGR rows, bottom-up, each row padded
/// to a 4-byte boundary. Alpha is composited over white (see the module docs).
#[must_use]
pub fn dib_bgr24(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = row_stride(width, 24);
    let image_size = stride * height as usize;

    let mut out = Vec::with_capacity(BITMAPINFOHEADER_SIZE as usize + image_size);
    out.extend_from_slice(&BITMAPINFOHEADER_SIZE.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes()); // Positive: bottom-up.
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&BI_RGB.to_le_bytes());
    out.extend_from_slice(&(image_size as u32).to_le_bytes());
    out.extend_from_slice(&PIXELS_PER_METRE.to_le_bytes());
    out.extend_from_slice(&PIXELS_PER_METRE.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    for row in rows_bottom_up(width, height, rgba) {
        let start = out.len();
        for pixel in row.as_chunks::<4>().0 {
            let alpha = u32::from(pixel[3]);
            out.push(over_white(pixel[2], alpha)); // B
            out.push(over_white(pixel[1], alpha)); // G
            out.push(over_white(pixel[0], alpha)); // R
        }
        out.resize(start + stride, 0); // The row's padding, and any row the buffer ran short of.
    }
    out
}

/// A `CF_DIBV5` block: `BITMAPV5HEADER` followed by 32-bit BGRA rows, bottom-up. Alpha is
/// straight (not premultiplied) and the colour space is declared sRGB.
#[must_use]
pub fn dib_v5_bgra32(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = row_stride(width, 32);
    let image_size = stride * height as usize;

    let mut out = Vec::with_capacity(BITMAPV5HEADER_SIZE as usize + image_size);
    out.extend_from_slice(&BITMAPV5HEADER_SIZE.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes()); // Positive: bottom-up.
    out.extend_from_slice(&1u16.to_le_bytes()); // bV5Planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bV5BitCount
    out.extend_from_slice(&BI_BITFIELDS.to_le_bytes());
    out.extend_from_slice(&(image_size as u32).to_le_bytes());
    out.extend_from_slice(&PIXELS_PER_METRE.to_le_bytes());
    out.extend_from_slice(&PIXELS_PER_METRE.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // bV5ClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // bV5ClrImportant
    // The masks say the bytes run B, G, R, A in memory (little-endian, so the mask reads the
    // other way round). BITMAPV4HEADER onwards carries them inline; nothing follows the header.
    out.extend_from_slice(&0x00FF_0000u32.to_le_bytes()); // bV5RedMask
    out.extend_from_slice(&0x0000_FF00u32.to_le_bytes()); // bV5GreenMask
    out.extend_from_slice(&0x0000_00FFu32.to_le_bytes()); // bV5BlueMask
    out.extend_from_slice(&0xFF00_0000u32.to_le_bytes()); // bV5AlphaMask
    out.extend_from_slice(&LCS_SRGB.to_le_bytes());
    out.resize(out.len() + 36, 0); // bV5Endpoints: unread for a named colour space.
    out.extend_from_slice(&0u32.to_le_bytes()); // bV5GammaRed
    out.extend_from_slice(&0u32.to_le_bytes()); // bV5GammaGreen
    out.extend_from_slice(&0u32.to_le_bytes()); // bV5GammaBlue
    out.extend_from_slice(&LCS_GM_IMAGES.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // bV5ProfileData
    out.extend_from_slice(&0u32.to_le_bytes()); // bV5ProfileSize
    out.extend_from_slice(&0u32.to_le_bytes()); // bV5Reserved

    for row in rows_bottom_up(width, height, rgba) {
        let start = out.len();
        for pixel in row.as_chunks::<4>().0 {
            out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
        out.resize(start + stride, 0); // Only reached when the buffer ran short.
    }
    out
}

/// A `CF_HDROP` block: a `DROPFILES` header followed by the paths as UTF-16, each terminated,
/// and one more terminator closing the list.
///
/// `None` when no path survived [`crate::paths::shell_path`], since an empty file list is worse than no file
/// list: the clipboard would offer Explorer a format with nothing in it.
#[must_use]
pub fn hdrop(paths: &[&Path]) -> Option<Vec<u8>> {
    let shell_paths: Vec<String> = paths
        .iter()
        .copied()
        .filter_map(crate::paths::shell_path)
        .collect();
    if shell_paths.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&DROPFILES_SIZE.to_le_bytes()); // pFiles: where the list starts.
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0u32.to_le_bytes()); // fNC: pt is client-relative, and unused here.
    out.extend_from_slice(&1u32.to_le_bytes()); // fWide: the paths are UTF-16.
    debug_assert_eq!(out.len(), DROPFILES_SIZE as usize);

    for path in &shell_paths {
        for unit in path.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes()); // The empty entry that ends the list.
    Some(out)
}

/// Bytes one row of `width` pixels occupies at `bits_per_pixel`, rounded up to the 4-byte
/// boundary every DIB row starts on.
fn row_stride(width: u32, bits_per_pixel: u32) -> usize {
    ((width as usize * bits_per_pixel as usize).div_ceil(32)) * 4
}

/// The image's rows in the order a DIB stores them: last row first. Rows the buffer doesn't
/// reach come back empty, so a caller padding to the stride fills them with black.
fn rows_bottom_up(width: u32, height: u32, rgba: &[u8]) -> impl Iterator<Item = &[u8]> {
    let source_stride = width as usize * 4;
    (0..height as usize).rev().map(move |y| {
        let start = y * source_stride;
        rgba.get(start..start + source_stride).unwrap_or(&[])
    })
}

/// One channel composited over a white background. `alpha` is 0–255.
fn over_white(channel: u8, alpha: u32) -> u8 {
    let blended = u32::from(channel) * alpha + 255 * (255 - alpha);
    ((blended + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two pixels wide, two tall: red opaque, green opaque / blue opaque, white opaque.
    fn opaque_2x2() -> Vec<u8> {
        vec![
            255, 0, 0, 255, // top-left red
            0, 255, 0, 255, // top-right green
            0, 0, 255, 255, // bottom-left blue
            255, 255, 255, 255, // bottom-right white
        ]
    }

    fn header_u32(dib: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(dib[offset..offset + 4].try_into().unwrap())
    }

    /// The header a `CF_DIB` consumer parses first. Every field here is one a consumer reads
    /// before it touches a pixel, and a wrong one means a paste that shows nothing.
    #[test]
    fn the_dib_header_says_24_bit_bottom_up_bgr() {
        let dib = dib_bgr24(2, 2, &opaque_2x2());
        assert_eq!(header_u32(&dib, 0), 40, "biSize");
        assert_eq!(header_u32(&dib, 4), 2, "biWidth");
        assert_eq!(header_u32(&dib, 8), 2, "biHeight, positive for bottom-up");
        assert_eq!(u16::from_le_bytes([dib[12], dib[13]]), 1, "biPlanes");
        assert_eq!(u16::from_le_bytes([dib[14], dib[15]]), 24, "biBitCount");
        assert_eq!(header_u32(&dib, 16), BI_RGB, "biCompression");
        assert_eq!(
            header_u32(&dib, 20),
            16,
            "biSizeImage: two 6-byte rows, each padded to 8"
        );
        assert_eq!(header_u32(&dib, 24), 3780, "biXPelsPerMeter");
    }

    /// Windows stores a DIB's last row first and its channels backwards. Getting either wrong
    /// pastes an upside-down or blue-for-red image, which no compiler catches.
    #[test]
    fn dib_rows_run_bottom_up_and_channels_run_bgr() {
        let dib = dib_bgr24(2, 2, &opaque_2x2());
        let pixels = &dib[40..];
        // First row stored is the image's bottom row: blue, then white.
        assert_eq!(&pixels[0..3], &[255, 0, 0], "blue as BGR");
        assert_eq!(&pixels[3..6], &[255, 255, 255], "white");
        assert_eq!(&pixels[6..8], &[0, 0], "the row's padding to 8 bytes");
        // Then the top row: red, then green.
        assert_eq!(&pixels[8..11], &[0, 0, 255], "red as BGR");
        assert_eq!(&pixels[11..14], &[0, 255, 0], "green");
    }

    /// A 24-bit row is 3 bytes per pixel, so any odd width needs padding to the 4-byte
    /// boundary. One pixel wide is the case that pads most: 3 bytes of colour, 1 of padding.
    #[test]
    fn odd_widths_pad_each_row_to_four_bytes() {
        let one_pixel_wide = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let dib = dib_bgr24(1, 2, &one_pixel_wide);
        assert_eq!(dib.len(), 40 + 8, "two 4-byte rows");
        assert_eq!(header_u32(&dib, 20), 8, "biSizeImage counts the padding");
        assert_eq!(&dib[40..44], &[60, 50, 40, 0], "bottom row, then its pad");
        assert_eq!(&dib[44..48], &[30, 20, 10, 0], "top row, then its pad");
    }

    /// The alpha decision: a consumer that ignores the fourth byte never sees one, because
    /// `CF_DIB` has none. Half-transparent red over white is the mid-tone that proves the
    /// blend actually ran rather than the channel being passed through.
    #[test]
    fn dib_composites_transparency_over_white() {
        let half_transparent_red = vec![255, 0, 0, 128];
        let dib = dib_bgr24(1, 1, &half_transparent_red);
        assert_eq!(
            &dib[40..43],
            &[127, 127, 255],
            "B, G at ~50% white; R stays"
        );

        let fully_transparent = vec![0, 0, 0, 0];
        let dib = dib_bgr24(1, 1, &fully_transparent);
        assert_eq!(&dib[40..43], &[255, 255, 255], "invisible pixels are white");
    }

    /// `CF_DIBV5`'s whole reason to exist is the alpha mask and the colour space. A consumer
    /// reads both out of the header, so they're pinned by offset.
    #[test]
    fn the_v5_header_declares_alpha_and_srgb() {
        let v5 = dib_v5_bgra32(2, 2, &opaque_2x2());
        assert_eq!(header_u32(&v5, 0), 124, "bV5Size");
        assert_eq!(u16::from_le_bytes([v5[14], v5[15]]), 32, "bV5BitCount");
        assert_eq!(header_u32(&v5, 16), BI_BITFIELDS, "bV5Compression");
        assert_eq!(header_u32(&v5, 40), 0x00FF_0000, "bV5RedMask");
        assert_eq!(header_u32(&v5, 44), 0x0000_FF00, "bV5GreenMask");
        assert_eq!(header_u32(&v5, 48), 0x0000_00FF, "bV5BlueMask");
        assert_eq!(header_u32(&v5, 52), 0xFF00_0000, "bV5AlphaMask");
        assert_eq!(header_u32(&v5, 56), LCS_SRGB, "bV5CSType");
        assert_eq!(header_u32(&v5, 108), LCS_GM_IMAGES, "bV5Intent");
        assert_eq!(
            v5.len(),
            124 + 16,
            "header plus four 4-byte pixels, no padding needed"
        );
    }

    /// V5 keeps the alpha it was given, unblended and unpremultiplied, in the fourth byte.
    #[test]
    fn v5_keeps_straight_alpha_in_bgra_order() {
        let half_transparent_red = vec![255, 0, 0, 128];
        let v5 = dib_v5_bgra32(1, 1, &half_transparent_red);
        assert_eq!(&v5[124..128], &[0, 0, 255, 128]);
    }

    /// The format choice itself: an opaque image gets one representation, because a second one
    /// carrying an all-255 alpha channel would double the memory to say nothing.
    #[test]
    fn v5_is_offered_only_when_alpha_is_doing_something() {
        assert!(windows_bitmaps(2, 2, &opaque_2x2()).dib_v5.is_none());

        let mut with_hole = opaque_2x2();
        with_hole[3] = 0;
        assert!(windows_bitmaps(2, 2, &with_hole).dib_v5.is_some());
    }

    /// A decoder that disagrees with its own dimensions must not take the app down mid-copy.
    #[test]
    fn a_short_buffer_pads_instead_of_panicking() {
        let one_pixel = vec![1, 2, 3, 255];
        let dib = dib_bgr24(2, 2, &one_pixel);
        assert_eq!(dib.len(), 40 + 2 * 8, "still a full 2x2 of rows");
        let v5 = dib_v5_bgra32(2, 2, &one_pixel);
        assert_eq!(v5.len(), 124 + 2 * 8);
    }

    /// The `DROPFILES` header is fixed except for one flag: `fWide`, which is what tells the
    /// shell the list is UTF-16 rather than ANSI. Reading it wrong turns a path into mojibake.
    #[test]
    fn the_hdrop_header_points_past_itself_and_says_wide() {
        let block = hdrop(&[Path::new(r"C:\a.jpg")]).unwrap();
        assert_eq!(header_u32(&block, 0), 20, "pFiles: the list starts at 20");
        assert_eq!(header_u32(&block, 4), 0, "pt.x");
        assert_eq!(header_u32(&block, 8), 0, "pt.y");
        assert_eq!(header_u32(&block, 12), 0, "fNC");
        assert_eq!(header_u32(&block, 16), 1, "fWide");
    }

    /// Every path terminates, and the list terminates again after the last one. A missing
    /// second terminator is the classic `CF_HDROP` bug: the shell reads past the block.
    #[test]
    fn hdrop_paths_are_utf16_and_double_terminated() {
        let block = hdrop(&[Path::new(r"C:\a.jpg"), Path::new(r"C:\b.png")]).unwrap();
        let units: Vec<u16> = block[20..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let expected: Vec<u16> = r"C:\a.jpg"
            .encode_utf16()
            .chain([0])
            .chain(r"C:\b.png".encode_utf16())
            .chain([0, 0])
            .collect();
        assert_eq!(units, expected);
    }

    /// Non-ASCII survives the trip: a Swedish photo folder is an ordinary case here, and a
    /// path outside the BMP still has to come out as a valid surrogate pair.
    #[test]
    fn hdrop_carries_paths_the_ansi_list_could_not() {
        let block = hdrop(&[Path::new(r"C:\Bilder\Lujza på ön 🎈.jpg")]).unwrap();
        let units: Vec<u16> = block[20..block.len() - 4]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            r"C:\Bilder\Lujza på ön 🎈.jpg"
        );
    }

    /// A drop list with nothing expressible in it is no drop list: offering Explorer an empty
    /// `CF_HDROP` is worse than offering it none.
    #[test]
    fn hdrop_is_absent_when_no_path_can_be_expressed() {
        assert_eq!(hdrop(&[Path::new(r"\\?\C:\photos\NUL.jpg")]), None);
        assert!(hdrop(&[]).is_none());
    }
}
