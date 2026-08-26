//! Read an image file's pixel dimensions without decoding it: enough to size
//! the window correctly before the first pixel paints.
//!
//! Used by the preview path, so the viewer window can auto-fit to the final
//! image size before the preview pixels are even uploaded to the GPU. The
//! preview is conceptually the image, just lower-res; the window should reach
//! its final size first.
//!
//! Everything here is pure Rust and identical on macOS, Windows, and Linux.
//! That is not just portability for its own sake: every tier reads headers with
//! the **same library the full decode uses**, so the dimensions this returns are
//! the dimensions that eventually paint. A tier can only be wrong where the
//! decode is also wrong, and it answers `None` exactly where the decode would
//! fail — no second window resize when the real pixels land.

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read, Seek};
use std::path::Path;

/// How much of a file's head to pull in one read. Comfortably more than any
/// JPEG's SOF + APP1/EXIF segments (typically both within the first 4 KB), and
/// enough for a WebP or TIFF whose IFD sits near the front. One read means one
/// round trip on a network share, which is what dominates on SMB.
const HEADER_PREFIX_BYTES: u64 = 64 * 1024;

/// Pixel dimensions of a source image with EXIF orientation applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

/// Read the display-space pixel dimensions of `path`, or `None` if nothing here
/// can answer for it.
///
/// Dispatches on the extension, because each family has a cheaper header route
/// than the one before it. On a slow network share each file open costs ~150 ms,
/// so the goal is **one** open per file whichever tier handles it.
///
/// | Tier | Formats | Reader | Why |
/// |------|---------|--------|-----|
/// | 1 | Camera RAW | `rawler` header-only parse | Same crate that develops the file, so the crop rect matches |
/// | 2 | PNG, GIF, BMP | `image::image_dimensions` | Tiny header, no EXIF to honour |
/// | 3 | JPEG | one 64 KB read, `image` for dims + `nom-exif` for orientation | Two parsers, one read, one round trip |
/// | 4 | WebP, TIFF, anything else | same 64 KB reader, format guessed from the magic bytes | Whatever the `image` crate opens, this sizes |
///
/// Used by both the dim prefetcher pool (in parallel) and as the lazy fallback
/// on the main thread when the prefetcher hasn't reached the requested index.
pub fn read_dimensions_fast(path: &Path) -> Option<Dimensions> {
    without_panicking(path, || read_dimensions_by_extension(path))
}

/// Run a header parse that a corrupt file could crash, and turn a crash into
/// "no dimensions".
///
/// Header parsers assert on geometry that has to nest and index into arrays
/// sized by numbers the file itself supplies, and a corrupt file supplies
/// whatever it likes: rawler alone carries an outright `panic!` on absurd
/// dimensions and asserts that the default crop sits inside the active area.
/// This runs on the launch path and on 16 prefetch threads, so a malformed
/// neighbour has to cost a `None` — not the process, and not a silently dead
/// worker that leaves the pool a thread short for the rest of the session.
fn without_panicking<T>(path: &Path, parse: impl FnOnce() -> Option<T>) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(parse)) {
        Ok(dimensions) => dimensions,
        Err(_) => {
            log::debug!("Header parse panicked for {}", path.display());
            None
        }
    }
}

fn read_dimensions_by_extension(path: &Path) -> Option<Dimensions> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some(e) if crate::decoding::is_raw_extension(e) => read_raw_dimensions(path),
        Some("png" | "gif" | "bmp") => read_via_image_crate(path),
        Some(e) if crate::decoding::is_jpeg_extension(e) => read_buffered_with_orientation(path),
        // WebP, TIFF, and any extension no backend claims. The buffered reader
        // guesses the format from the magic bytes, so it answers for whatever
        // the `image` crate can open and `None` for the rest.
        _ => read_buffered_with_orientation(path),
    }
}

/// Tier 1: camera RAW, via `rawler`'s header-only parse.
///
/// `raw_image(.., dummy = true)` walks the container and fills in the geometry
/// (sensor size, active area, default crop) but allocates no pixel buffer and
/// runs no decompression, so it costs a header read rather than a decode.
/// Orientation comes from the decoder's metadata, the same place `decoding::raw`
/// reads it — rawler hard-codes `RawImage.orientation` to `Normal`, so the field
/// on the image is not the answer.
fn read_raw_dimensions(path: &Path) -> Option<Dimensions> {
    use rawler::decoders::RawDecodeParams;
    use rawler::rawsource::RawSource;

    let src = RawSource::new(path).ok()?;
    let decoder = rawler::get_decoder(&src).ok()?;
    let params = RawDecodeParams::default();
    let raw = decoder.raw_image(&src, &params, true).ok()?;
    // rawler counts pixels in `usize`. A rectangle that doesn't fit a `u32` is
    // a corrupt header, and dropping it is better than a truncating cast.
    let rect =
        |r: rawler::imgop::Rect| Some((u32::try_from(r.d.w).ok()?, u32::try_from(r.d.h).ok()?));
    let (w, h) = developed_raw_dimensions(
        u32::try_from(raw.width).ok()?,
        u32::try_from(raw.height).ok()?,
        raw.crop_area.and_then(rect),
        raw.active_area.and_then(rect),
    );
    let orientation = decoder
        .raw_metadata(&src, &params)
        .ok()
        .and_then(|m| m.exif.orientation)
        .unwrap_or(1);
    let (width, height) = oriented(w, h, orientation);
    (width > 0 && height > 0).then_some(Dimensions { width, height })
}

/// What the RAW develop ends on, given the sensor geometry.
///
/// `decoding::raw` runs rawler's `CropActiveArea` (landing on the active area)
/// and then its own `apply_default_crop` (landing on `crop_area`), so the last
/// rectangle that exists is the one the user sees. Pure, so the rule stays
/// checkable without a RAW file.
fn developed_raw_dimensions(
    sensor_w: u32,
    sensor_h: u32,
    crop_area: Option<(u32, u32)>,
    active_area: Option<(u32, u32)>,
) -> (u32, u32) {
    crop_area.or(active_area).unwrap_or((sensor_w, sensor_h))
}

/// Tier 2: PNG/GIF/BMP. `image::image_dimensions` reads only the header (IHDR
/// for PNG, Logical Screen Descriptor for GIF, BMP file header). None of the
/// three carries orientation in a header the decode honours either, so there's
/// nothing to swap.
fn read_via_image_crate(path: &Path) -> Option<Dimensions> {
    let (w, h) = image::image_dimensions(path).ok()?;
    Some(Dimensions {
        width: w,
        height: h,
    })
}

/// Tiers 3 and 4: one open, one read of up to 64 KB, both answers parsed out of
/// that buffer — dimensions through the `image` crate, EXIF orientation through
/// `nom-exif`. Neither parser does further I/O, so it's a single round trip.
///
/// Orientation deliberately comes from `nom-exif` rather than `image`'s own
/// `ImageDecoder::orientation`, because `nom-exif` is what
/// `decoding::orientation` uses on the real decode. Where the two disagree
/// (WebP, whose EXIF chunk `image` reads and `nom-exif` doesn't), matching the
/// decode is what keeps the window from resizing twice.
fn read_buffered_with_orientation(path: &Path) -> Option<Dimensions> {
    if let Some(bytes) = read_prefix(path, HEADER_PREFIX_BYTES)
        && let Some((w, h)) = dimensions_from(Cursor::new(bytes.as_slice()))
    {
        let orientation = exif_orientation(Cursor::new(bytes.as_slice())).unwrap_or(1);
        let (width, height) = oriented(w, h, orientation);
        return Some(Dimensions { width, height });
    }
    // The header didn't fit in the prefix. TIFF is the case that matters:
    // plenty of writers park the IFD at the end of the file. Pay a second open
    // and let both parsers seek to it.
    let (w, h) = dimensions_from(BufReader::new(File::open(path).ok()?))?;
    let orientation = File::open(path)
        .ok()
        .and_then(|f| exif_orientation(BufReader::new(f)))
        .unwrap_or(1);
    let (width, height) = oriented(w, h, orientation);
    Some(Dimensions { width, height })
}

/// Read up to `limit` bytes from the head of `path`. `None` if the file can't
/// be opened or read at all.
fn read_prefix(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(limit as usize);
    file.take(limit).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// Pixel dimensions straight from the container header, format guessed from the
/// magic bytes. No orientation applied.
fn dimensions_from<R: BufRead + Seek>(reader: R) -> Option<(u32, u32)> {
    image::ImageReader::new(reader)
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// EXIF orientation tag (0x0112), or `None` when the file has no EXIF segment,
/// no orientation in it, or a value that isn't an integer. Same parser and same
/// tag the decode reads in `decoding::orientation`.
fn exif_orientation<R: Read + Seek>(reader: R) -> Option<u16> {
    let mut parser = nom_exif::MediaParser::new();
    let source = nom_exif::MediaSource::seekable(reader).ok()?;
    let iter: nom_exif::ExifIter = parser.parse_exif(source).ok()?;
    let exif: nom_exif::Exif = iter.into();
    match exif.get(nom_exif::ExifTag::Orientation)? {
        nom_exif::EntryValue::U16(n) => Some(*n),
        nom_exif::EntryValue::I16(n) => u16::try_from(*n).ok(),
        nom_exif::EntryValue::U32(n) => u16::try_from(*n).ok(),
        nom_exif::EntryValue::U8(n) => Some(u16::from(*n)),
        _ => None,
    }
}

/// Display-space dimensions for a stored size plus an EXIF orientation.
/// Orientations 5-8 turn the image a quarter turn, which swaps the two. The
/// spec defines 1-8; cameras occasionally write something else, and anything
/// off-spec is treated as upright rather than guessed at.
fn oriented(width: u32, height: u32, orientation: u16) -> (u32, u32) {
    if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A scratch file path unique to this process and test name. Tests that
    /// need one clean up after themselves.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prvw-metadata-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join(name)
    }

    fn fixture(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rel)
    }

    #[test]
    fn oriented_swaps_only_for_quarter_turns() {
        for upright in [1, 2, 3, 4] {
            assert_eq!(oriented(4000, 3000, upright), (4000, 3000), "{upright}");
        }
        for turned in [5, 6, 7, 8] {
            assert_eq!(oriented(4000, 3000, turned), (3000, 4000), "{turned}");
        }
        // Cameras do write garbage here; treat anything off-spec as upright.
        assert_eq!(oriented(4000, 3000, 0), (4000, 3000));
        assert_eq!(oriented(4000, 3000, 9), (4000, 3000));
    }

    #[test]
    fn developed_raw_dimensions_prefer_the_default_crop() {
        // Sensor is bigger than the active area, which is bigger than the crop.
        // The develop ends on the crop, so that's what the window must fit.
        assert_eq!(
            developed_raw_dimensions(6024, 4024, Some((6000, 4000)), Some((6016, 4016))),
            (6000, 4000)
        );
        // No crop tag: the develop ends on the active area.
        assert_eq!(
            developed_raw_dimensions(6024, 4024, None, Some((6016, 4016))),
            (6016, 4016)
        );
        // Neither: the full sensor.
        assert_eq!(
            developed_raw_dimensions(6024, 4024, None, None),
            (6024, 4024)
        );
    }

    #[test]
    fn raw_dimensions_match_what_the_develop_produces() {
        let dng = fixture("raw/synthetic-bayer-128.dng");
        assert_eq!(
            read_raw_dimensions(&dng),
            Some(Dimensions {
                width: 128,
                height: 128
            })
        );
    }

    #[test]
    fn read_dimensions_fast_routes_raw_off_the_apple_path() {
        let dng = fixture("raw/synthetic-bayer-128.dng");
        assert_eq!(
            read_dimensions_fast(&dng),
            Some(Dimensions {
                width: 128,
                height: 128
            })
        );
    }

    #[test]
    fn a_truncated_raw_returns_none_instead_of_panicking() {
        let bytes = std::fs::read(fixture("raw/synthetic-bayer-128.dng")).expect("fixture");
        let path = scratch("truncated.dng");
        std::fs::write(&path, &bytes[..512]).expect("write");
        assert_eq!(read_raw_dimensions(&path), None);
        assert_eq!(read_dimensions_fast(&path), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn garbage_never_panics_whatever_the_extension_claims() {
        // A byte pattern that is not any image format, plus a plausible TIFF
        // magic prefix with nonsense behind it — the shape most likely to walk
        // a header parser off a cliff.
        let payloads: [Vec<u8>; 3] = [Vec::new(), (0u8..=255).cycle().take(4096).collect(), {
            let mut v = vec![0x49, 0x49, 0x2a, 0x00];
            v.extend((0u8..=255).cycle().take(4092));
            v
        }];
        for (i, payload) in payloads.iter().enumerate() {
            for ext in [
                "dng", "arw", "cr2", "cr3", "nef", "raf", "jpg", "jfif", "tif", "webp", "png",
                "gif", "bmp", "xyz",
            ] {
                let path = scratch(&format!("garbage-{i}.{ext}"));
                std::fs::write(&path, payload).expect("write");
                // The contract is "no dimensions", never a panic.
                let _ = read_dimensions_fast(&path);
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    #[test]
    fn the_catch_all_arm_answers_for_tiff_without_imageio() {
        let path = scratch("plain.tiff");
        image::RgbImage::new(40, 20)
            .save(&path)
            .expect("encode tiff");
        assert_eq!(
            read_buffered_with_orientation(&path),
            Some(Dimensions {
                width: 40,
                height: 20
            })
        );
        assert_eq!(
            read_dimensions_fast(&path),
            Some(Dimensions {
                width: 40,
                height: 20
            })
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn every_jpeg_extension_reaches_the_jpeg_tier() {
        let bytes = std::fs::read(fixture("p3_red_64x64.jpg")).expect("fixture");
        for ext in ["jpg", "jpeg", "jpe", "jfif", "JPG"] {
            let path = scratch(&format!("photo.{ext}"));
            std::fs::write(&path, &bytes).expect("write");
            assert_eq!(
                read_dimensions_fast(&path),
                Some(Dimensions {
                    width: 64,
                    height: 64
                }),
                "{ext}"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn a_header_parser_that_panics_yields_no_dimensions() {
        // The default hook would print the backtrace of a panic we're catching
        // on purpose. Silenced only for the duration of the call.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = without_panicking::<Dimensions>(Path::new("corrupt.dng"), || {
            panic!("a header parser walked off the end of the file")
        });
        std::panic::set_hook(previous);
        assert_eq!(caught, None);
    }

    #[test]
    fn a_missing_file_is_none_on_every_tier() {
        for ext in ["dng", "jpg", "png", "tiff", "xyz"] {
            let path = scratch(&format!("does-not-exist.{ext}"));
            assert_eq!(read_dimensions_fast(&path), None, "{ext}");
        }
    }
}
