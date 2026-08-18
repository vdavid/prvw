//! Read pixel dimensions of an image file via `ImageIO` (CGImageSource).
//! Does not decode pixels — just parses metadata headers, so it's fast
//! (~1 ms per file) and uniform across JPEG, PNG, HEIC, RAW, etc.
//!
//! Used by the preview path so the viewer window can auto-fit to the
//! final image size before the preview pixels are even uploaded to the GPU.
//! The preview is conceptually the image, just lower-res; the window
//! should reach its final size first.

use std::ffi::c_void;
use std::path::Path;

// CoreFoundation / ImageIO opaque pointer types.
#[allow(non_camel_case_types)]
type CFTypeRef = *const c_void;
#[allow(non_camel_case_types)]
type CFStringRef = *const c_void;
#[allow(non_camel_case_types)]
type CFURLRef = *const c_void;
#[allow(non_camel_case_types)]
type CFDictionaryRef = *const c_void;
#[allow(non_camel_case_types)]
type CFNumberRef = *const c_void;
#[allow(non_camel_case_types)]
type CFAllocatorRef = *const c_void;
#[allow(non_camel_case_types)]
type CGImageSourceRef = *const c_void;

/// `kCFStringEncodingUTF8`.
const CF_STRING_ENCODING_UTF8: u32 = 0x08000100;
/// `kCFNumberSInt64Type` — 64-bit signed integer.
const CF_NUMBER_SINT64_TYPE: i64 = 4;

unsafe extern "C" {
    fn CFStringCreateWithBytes(
        allocator: CFAllocatorRef,
        bytes: *const u8,
        num_bytes: isize,
        encoding: u32,
        external_repr: bool,
    ) -> CFStringRef;

    fn CFURLCreateWithFileSystemPath(
        allocator: CFAllocatorRef,
        file_path: CFStringRef,
        path_style: usize,
        is_directory: bool,
    ) -> CFURLRef;

    fn CGImageSourceCreateWithURL(url: CFURLRef, options: CFDictionaryRef) -> CGImageSourceRef;

    fn CGImageSourceCopyPropertiesAtIndex(
        src: CGImageSourceRef,
        index: usize,
        options: CFDictionaryRef,
    ) -> CFDictionaryRef;

    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;

    fn CFNumberGetValue(num: CFNumberRef, type_: i64, value_ptr: *mut c_void) -> bool;

    fn CFRelease(cf: CFTypeRef);

    // Global constants from ImageIO: CFStringRefs naming the property keys.
    static kCGImagePropertyPixelWidth: CFStringRef;
    static kCGImagePropertyPixelHeight: CFStringRef;
    static kCGImagePropertyOrientation: CFStringRef;
}

/// `kCFURLPOSIXPathStyle`.
const CF_URL_POSIX_PATH_STYLE: usize = 0;

/// Pixel dimensions of a source image with EXIF orientation applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

/// Read the pixel width, height, and EXIF orientation of `path`. Swaps
/// width/height for rotated orientations (5–8) so callers get the
/// display-space dimensions.
pub fn read_dimensions(path: &Path) -> Option<Dimensions> {
    let path_str = path.to_str()?;
    unsafe {
        let cf_path = CFStringCreateWithBytes(
            std::ptr::null(),
            path_str.as_ptr(),
            path_str.len() as isize,
            CF_STRING_ENCODING_UTF8,
            false,
        );
        if cf_path.is_null() {
            return None;
        }
        let url = CFURLCreateWithFileSystemPath(
            std::ptr::null(),
            cf_path,
            CF_URL_POSIX_PATH_STYLE,
            false,
        );
        CFRelease(cf_path);
        if url.is_null() {
            return None;
        }
        let source = CGImageSourceCreateWithURL(url, std::ptr::null());
        CFRelease(url);
        if source.is_null() {
            return None;
        }
        let props = CGImageSourceCopyPropertiesAtIndex(source, 0, std::ptr::null());
        CFRelease(source);
        if props.is_null() {
            return None;
        }

        let width = dict_i64(props, kCGImagePropertyPixelWidth).and_then(|v| u32::try_from(v).ok());
        let height =
            dict_i64(props, kCGImagePropertyPixelHeight).and_then(|v| u32::try_from(v).ok());
        let orientation = dict_i64(props, kCGImagePropertyOrientation).unwrap_or(1);
        CFRelease(props);

        let (w, h) = match (width, height) {
            (Some(w), Some(h)) => (w, h),
            _ => return None,
        };
        // EXIF orientations 5–8 rotate by 90° or 270°, swapping dims.
        let (w, h) = if (5..=8).contains(&orientation) {
            (h, w)
        } else {
            (w, h)
        };
        Some(Dimensions {
            width: w,
            height: h,
        })
    }
}

/// Read a signed integer out of a `CFDictionary` keyed by a `CFStringRef`
/// constant. Returns `None` if the key is absent or the value isn't a
/// `CFNumber` convertible to `i64`.
unsafe fn dict_i64(dict: CFDictionaryRef, key: CFStringRef) -> Option<i64> {
    unsafe {
        let value = CFDictionaryGetValue(dict, key);
        if value.is_null() {
            return None;
        }
        let mut out: i64 = 0;
        let ok = CFNumberGetValue(
            value,
            CF_NUMBER_SINT64_TYPE,
            (&mut out) as *mut i64 as *mut c_void,
        );
        if ok { Some(out) } else { None }
    }
}

/// Three-tier dispatcher chosen by extension. On a slow network share
/// each file open is ~150 ms RTT, so the goal is to do **one** open per
/// file regardless of which tier handles it.
///
/// | Tier | Formats | Reader | Why |
/// |------|---------|--------|-----|
/// | 1 | PNG, GIF, BMP | `image::image_dimensions` | No EXIF needed, header is tiny, pure-Rust no XPC overhead |
/// | 2 | JPEG | open once, parse dim + EXIF orientation from same 64 KB buffer | Two parsers, one read, one network RTT |
/// | 3 | RAW, HEIC, WebP, TIFF, others | `read_dimensions` (ImageIO) | Format coverage is the priority; ImageIO handles them all in one pass |
///
/// Used by both the dim prefetcher pool (parallel) and as the lazy
/// fallback on the main thread when the prefetcher hasn't reached the
/// requested index yet.
pub fn read_dimensions_fast(path: &Path) -> Option<Dimensions> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png" | "gif" | "bmp") => read_via_image_crate(path),
        Some("jpg" | "jpeg") => read_jpeg_with_orientation(path),
        // RAW, HEIC, WebP, TIFF, and any unknown extension → ImageIO.
        _ => read_dimensions(path),
    }
}

/// Tier 1: PNG/GIF/BMP. `image::image_dimensions` reads only the header
/// (IHDR for PNG, Logical Screen Descriptor for GIF, BMP file header).
/// No EXIF orientation is meaningful for these formats — they don't
/// store orientation in standard headers.
fn read_via_image_crate(path: &Path) -> Option<Dimensions> {
    let (w, h) = image::image_dimensions(path).ok()?;
    Some(Dimensions {
        width: w,
        height: h,
    })
}

/// Tier 2: JPEG. Open once, read 64 KB into RAM, parse both dimensions
/// (via the `image` crate) and EXIF orientation (via `nom-exif`) from
/// the same in-memory buffer. Single network RTT for both pieces of
/// data.
///
/// 64 KB is comfortably more than any JPEG's SOF + APP1/EXIF segments
/// — typically both are within the first 4 KB.
fn read_jpeg_with_orientation(path: &Path) -> Option<Dimensions> {
    use std::io::{Cursor, Read};

    // One open, one read of up to 64 KB, one close. ~1 SMB RTT.
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(64 * 1024);
    file.take(64 * 1024).read_to_end(&mut bytes).ok()?;

    // Pure-Rust dim parse from the buffer (no further I/O).
    let cursor = Cursor::new(&bytes);
    let reader =
        image::ImageReader::with_format(std::io::BufReader::new(cursor), image::ImageFormat::Jpeg);
    let (mut w, mut h) = reader.into_dimensions().ok()?;

    // Pure-Rust EXIF parse from the same buffer, through nom-exif's `MediaParser` /
    // `MediaSource` API. The parser reuses internal buffers across files, which buys us
    // nothing for a single 64 KB buffer, but it's the only API left: nom-exif 3 dropped the
    // one-shot `parse_jpeg_exif`.
    let orient = (|| -> Option<u16> {
        let mut parser = nom_exif::MediaParser::new();
        let ms = nom_exif::MediaSource::seekable(Cursor::new(&bytes)).ok()?;
        let iter: nom_exif::ExifIter = parser.parse_exif(ms).ok()?;
        let exif: nom_exif::Exif = iter.into();
        match exif.get(nom_exif::ExifTag::Orientation)? {
            nom_exif::EntryValue::U16(n) => Some(*n),
            nom_exif::EntryValue::I16(n) => u16::try_from(*n).ok(),
            nom_exif::EntryValue::U32(n) => u16::try_from(*n).ok(),
            _ => None,
        }
    })()
    .unwrap_or(1);

    if (5..=8).contains(&orient) {
        std::mem::swap(&mut w, &mut h);
    }
    Some(Dimensions {
        width: w,
        height: h,
    })
}
