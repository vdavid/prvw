//! Finding the gain map in a JPEG: which metadata flavor the file carries, and which of its
//! appended images is the map (`docs/specs/hdr-images.md`, section 4).
//!
//! Only ever called with HDR active. On an SDR display, with HDR off, or for a byte consumer
//! (`OutputDepth::Rgba8`), `jpeg::decode` never gets here, so a gain-map JPEG costs exactly what
//! it did before.

pub mod mpf;
pub mod xmp;

use std::ops::Range;

use crate::color::hdr::fragment::GainMapInputs;
use crate::color::hdr::gain_map::{GainMapParams, GainMapSource, GainMapSpaces, MapTurn, MapView};

/// A gain-mapped image's map and everything the shader's mode 2 needs to apply it. Rides on
/// `PixelEncoding::GainMapped`, so the cache, the crossfade, and the renderer all carry it with
/// the base pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct GainMapPayload {
    pub width: u32,
    pub height: u32,
    /// The map's raw values, RGBA8, a single-channel map replicated into R, G, and B. Data, not
    /// color: never color-managed. Kept in the file's frame, unturned; `turn` says how the base
    /// was turned, and the sampler goes back through it (`MapTurn` says why).
    pub rgba: Vec<u8>,
    pub params: GainMapParams,
    pub source: GainMapSource,
    pub spaces: GainMapSpaces,
    pub turn: MapTurn,
}

impl GainMapPayload {
    /// The shader inputs for a base of `image_size`.
    pub fn inputs(&self, image_size: [u32; 2]) -> GainMapInputs<'_> {
        GainMapInputs {
            params: self.params,
            map: MapView {
                width: self.width,
                height: self.height,
                rgba: &self.rgba,
                turn: self.turn,
            },
            image_size,
            p3_to_app: self.spaces.p3_to_app,
            app_to_display: self.spaces.app_to_display,
            p3_to_display: self.spaces.p3_to_display,
        }
    }
}

/// What the primary image's own headers say, as `zune-jpeg` parsed them.
pub struct PrimaryHeaders<'a> {
    pub xmp: Option<&'a [u8]>,
    pub exif: Option<&'a [u8]>,
    /// The MPF payload after `MPF\0`, and the file offset of its first byte.
    pub mpf: Option<(&'a [u8], usize)>,
    /// The payloads of the primary's ISO 21496-1 APP2 segments (after the identifier). The
    /// usual one is four version bytes, which only says "this file has an ISO gain map".
    pub iso: Vec<&'a [u8]>,
}

/// A gain map located in the file, with its parameters.
#[derive(Debug)]
pub struct Located {
    pub params: GainMapParams,
    pub source: GainMapSource,
    /// The gain-map image's own JPEG bytes, as a range into the file.
    pub map: Range<usize>,
    /// The gain-map image's ICC profile, which describes the alternate rendition's color space
    /// when the metadata says to apply the gain there.
    pub map_icc: Option<Vec<u8>>,
}

/// One appended image's headers.
struct Candidate {
    range: Range<usize>,
    iso: Option<GainMapParams>,
    has_iso_segment: bool,
    hdrgm: Option<(GainMapParams, GainMapSource)>,
    apple_tagged: bool,
    icc: Option<Vec<u8>>,
}

/// Find the gain map, or `None` for a plain JPEG (or one whose gain map can't be read, which is
/// shown as the plain JPEG it also is). Metadata precedence: ISO 21496-1, then Ultra HDR XMP,
/// then Apple.
pub fn locate(file: &[u8], primary: &PrimaryHeaders) -> Option<Located> {
    let primary_xmp = primary.xmp.map(String::from_utf8_lossy);
    let iso_signal = !primary.iso.is_empty();
    let hdrgm_signal = primary_xmp.as_deref().is_some_and(xmp::mentions_hdrgm);
    let apple = primary.exif.and_then(crate::color::hdr::parse_apple);
    if !iso_signal && !hdrgm_signal && apple.is_none() {
        return None;
    }

    let candidates = candidate_ranges(file, primary, primary_xmp.as_deref())
        .into_iter()
        .filter_map(|range| inspect(file, range))
        .collect::<Vec<_>>();

    // ISO 21496-1 first: an APP2 segment on the map image, then Apple's XMP rendering of the
    // same fields on it, then (rarely) full parameters on the primary.
    if let Some(c) = candidates.iter().find(|c| c.iso.is_some()) {
        return Some(located(c, c.iso?, GainMapSource::Iso21496));
    }
    let xmp_flavor = |source| {
        candidates.iter().find_map(|c| match c.hdrgm {
            Some((params, s)) if s == source => Some(located(c, params, s)),
            _ => None,
        })
    };
    if let Some(found) = xmp_flavor(GainMapSource::Iso21496) {
        return Some(found);
    }
    let primary_iso = primary
        .iso
        .iter()
        .find_map(|p| crate::color::hdr::parse_iso21496(p));
    if let Some(params) = primary_iso
        && let Some(c) = candidates
            .iter()
            .find(|c| c.has_iso_segment)
            .or(candidates.first())
    {
        return Some(located(c, params, GainMapSource::Iso21496));
    }
    if let Some(found) = xmp_flavor(GainMapSource::UltraHdrXmp) {
        return Some(found);
    }
    if let Some(params) = apple
        && let Some(c) = candidates.iter().find(|c| c.apple_tagged)
    {
        return Some(located(c, params, GainMapSource::Apple));
    }
    log::debug!(
        "JPEG signals a gain map (ISO {iso_signal}, Ultra HDR {hdrgm_signal}, Apple {}) but none \
         of its {} appended images is one",
        apple.is_some(),
        candidates.len()
    );
    None
}

fn located(c: &Candidate, params: GainMapParams, source: GainMapSource) -> Located {
    Located {
        params,
        source,
        map: c.range.clone(),
        map_icc: c.icc.clone(),
    }
}

/// The appended images worth looking at: every MPF entry but the primary, or, with no MPF, the
/// `GContainer` directory's gain-map item.
fn candidate_ranges(
    file: &[u8],
    primary: &PrimaryHeaders,
    primary_xmp: Option<&str>,
) -> Vec<Range<usize>> {
    if let Some((index, offset)) = primary.mpf {
        let images = mpf::parse(index, offset, file.len());
        let appended: Vec<_> = images
            .into_iter()
            .map(|i| i.range)
            .filter(|r| r.start != 0)
            .collect();
        if !appended.is_empty() {
            return appended;
        }
    }
    // GContainer lists items in file order and gives the length of each one after the primary,
    // so the gain map ends where the file does.
    let items = primary_xmp.map(xmp::container_items).unwrap_or_default();
    let Some(at) = items.iter().position(|i| i.semantic == "GainMap") else {
        return Vec::new();
    };
    let Some(tail) = items[at..]
        .iter()
        .try_fold(0usize, |sum, i| sum.checked_add(i.length?))
    else {
        return Vec::new();
    };
    let length = items[at].length.unwrap_or(0);
    match file.len().checked_sub(tail) {
        Some(start) if start > 0 && length > 0 => std::iter::once(start..start + length).collect(),
        _ => Vec::new(),
    }
}

/// Read one appended image's headers (no pixels).
fn inspect(file: &[u8], range: Range<usize>) -> Option<Candidate> {
    let bytes = file.get(range.clone())?;
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        log::debug!("Appended image at {} isn't a JPEG", range.start);
        return None;
    }
    let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(bytes));
    if let Err(e) = decoder.decode_headers() {
        log::debug!(
            "Appended image at {} has unreadable headers: {e}",
            range.start
        );
        return None;
    }
    let info = decoder.info()?;
    let iso = info
        .gain_map_info
        .iter()
        .find_map(|g| crate::color::hdr::parse_iso21496(&g.data));
    let xmp = decoder
        .xmp()
        .map(|x| String::from_utf8_lossy(x).into_owned());
    Some(Candidate {
        range,
        iso,
        has_iso_segment: !info.gain_map_info.is_empty(),
        hdrgm: xmp.as_deref().and_then(xmp::parse_hdrgm),
        apple_tagged: xmp.as_deref().is_some_and(xmp::is_apple_gain_map),
        icc: decoder.icc_profile(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::hdr::gain_map::GainCurve;

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/hdr")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The headers `jpeg::decode` hands over, read the same way.
    fn locate_in(file: &[u8]) -> Option<Located> {
        let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(file));
        decoder.decode_headers().expect("primary headers");
        let info = decoder.info().expect("info");
        let mpf = info
            .multi_picture_information
            .as_deref()
            .zip(info.multi_picture_information_offset.map(|o| o as usize));
        let primary = PrimaryHeaders {
            xmp: decoder.xmp().map(Vec::as_slice),
            exif: decoder.exif().map(Vec::as_slice),
            mpf,
            iso: info
                .gain_map_info
                .iter()
                .map(|g| g.data.as_slice())
                .collect(),
        };
        locate(file, &primary)
    }

    /// The real iPhone Air file carries both flavors iOS 18 writes. ISO 21496-1 wins, and the
    /// Apple reading of the same file is there too, so the two can be compared.
    #[test]
    fn the_iphone_fixture_has_iso_and_apple_metadata_and_iso_wins() {
        let file = fixture("iphone-air-gain-map-776.jpg");
        let found = locate_in(&file).expect("the iPhone file has a gain map");
        assert_eq!(found.source, GainMapSource::Iso21496);
        assert!(found.params.alternate_headroom > found.params.base_headroom);
        assert!(file[found.map.clone()].starts_with(&[0xFF, 0xD8]));

        let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(file.as_slice()));
        decoder.decode_headers().expect("headers");
        let apple = crate::color::hdr::parse_apple(decoder.exif().expect("EXIF"))
            .expect("the maker note carries Apple's headroom");
        // Apple's maker-note formula lands on the headroom the same file states in ISO terms
        // (1.767799 stops), which is what anchors its constants.
        assert!(
            (apple.alternate_headroom - found.params.alternate_headroom).abs() < 2e-3,
            "Apple {} vs ISO {} stops",
            apple.alternate_headroom,
            found.params.alternate_headroom
        );
        assert!((found.params.alternate_headroom - 1.7678).abs() < 1e-3);
    }

    #[test]
    fn the_ultra_hdr_fixture_reads_its_xmp_parameters() {
        let file = fixture("ultra-hdr-xmp-64x48.jpg");
        let found = locate_in(&file).expect("gain map");
        assert_eq!(found.source, GainMapSource::UltraHdrXmp);
        assert_eq!(found.params.channels[0].max, 2.0);
        assert_eq!(found.params.alternate_headroom, 2.0);
        assert_eq!(found.map.end, file.len(), "the map is appended last");
    }

    #[test]
    fn the_iso_fixture_reads_per_channel_parameters_from_the_map() {
        let found = locate_in(&fixture("iso-21496-64x48.jpg")).expect("gain map");
        assert_eq!(found.source, GainMapSource::Iso21496);
        assert_eq!(found.params.channels.map(|c| c.max), [2.0, 1.5, 1.0]);
        assert_eq!(found.params.channels[1].min, -0.5);
    }

    #[test]
    fn the_apple_fixture_reads_the_maker_note_headroom() {
        let found = locate_in(&fixture("apple-64x48.jpg")).expect("gain map");
        assert_eq!(found.source, GainMapSource::Apple);
        // maker33 = 1.01, maker48 = 0.5: -0.303 × 0.5 + 2.303 stops.
        assert!((found.params.alternate_headroom - 2.1515).abs() < 1e-5);
        assert_eq!(found.params.curve, GainCurve::Apple);
    }

    /// No MPF index: the `GContainer` directory's lengths still find the map.
    #[test]
    fn without_mpf_the_gcontainer_directory_finds_the_map() {
        let mut file = fixture("ultra-hdr-xmp-64x48.jpg");
        let at = file
            .windows(4)
            .position(|w| w == b"MPF\0")
            .expect("the fixture has MPF");
        file[at..at + 4].copy_from_slice(b"XXX\0");
        let found = locate_in(&file).expect("gain map through GContainer");
        assert_eq!(found.source, GainMapSource::UltraHdrXmp);
        assert_eq!(found.map.end, file.len());
    }

    /// A gain-map file cut short, or with its index overwritten, is file bytes like any other:
    /// the locator answers (usually "no gain map") and never panics.
    #[test]
    fn truncated_or_corrupt_gain_map_files_never_panic() {
        for name in [
            "ultra-hdr-xmp-64x48.jpg",
            "iso-21496-64x48.jpg",
            "apple-64x48.jpg",
        ] {
            let file = fixture(name);
            for len in (0..file.len()).step_by(7) {
                let cut = &file[..len];
                let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(cut));
                if decoder.decode_headers().is_ok() {
                    let _ = locate_in(cut);
                }
            }
            let mpf = file.windows(4).position(|w| w == b"MPF\0").expect("MPF");
            for at in mpf + 4..mpf + 4 + 82 {
                let mut corrupt = file.clone();
                corrupt[at] ^= 0xA5;
                if let Some(found) = locate_in(&corrupt) {
                    assert!(found.map.end <= corrupt.len());
                }
            }
        }
    }

    #[test]
    fn a_jpeg_with_no_gain_map_signal_is_plain() {
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .expect("encode");
        assert!(locate_in(&jpeg).is_none());
    }
}
