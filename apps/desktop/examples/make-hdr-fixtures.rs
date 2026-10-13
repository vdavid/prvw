//! Writes the synthetic HDR fixtures in `tests/fixtures/hdr/` (see the `README.md` there).
//!
//! ```sh
//! cd apps/desktop
//! cargo run --example make-hdr-fixtures
//! ```
//!
//! Each gain-map JPEG is a 64×48 SDR base plus a 16×12 gain map appended after it, found through
//! an MPF index in the base, the way libultrahdr and iOS lay them out:
//!
//! - `ultra-hdr-xmp-64x48.jpg`: Ultra HDR v1. `hdrgm:Version` and a `GContainer` directory in
//!   the primary's XMP, the parameters in the map's XMP, a single-channel map.
//! - `ultra-hdr-xmp-orientation-6.jpg`: the same, with EXIF orientation 6 on the primary.
//! - `iso-21496-64x48.jpg`: ISO 21496-1. A version-only APP2 on the primary, the parameters in an
//!   APP2 on the map, a three-channel map with per-channel parameters, and a Display P3 base.
//! - `apple-64x48.jpg`: Apple's older flavor. Headroom in an Apple maker note in the primary's
//!   EXIF, a map tagged `urn:com:apple:photo:2020:aux:hdrgainmap` and nothing else.
//!
//! Self-made fixtures risk a misunderstanding shared by this writer and the app's reader, which
//! is why the real iPhone file sits next to them.

use std::path::Path;

use image::ExtendedColorType;
use image::codecs::jpeg::JpegEncoder;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const MAP_WIDTH: u32 = 16;
const MAP_HEIGHT: u32 = 12;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hdr");
    let write = |name: &str, bytes: Vec<u8>| {
        let path = dir.join(name);
        std::fs::write(&path, &bytes).expect("write fixture");
        println!("{} ({} bytes)", path.display(), bytes.len());
    };

    write("ultra-hdr-xmp-64x48.jpg", ultra_hdr(None));
    write("ultra-hdr-xmp-orientation-6.jpg", ultra_hdr(Some(6)));
    write("iso-21496-64x48.jpg", iso_21496());
    write("apple-64x48.jpg", apple());
}

/// A base with a horizontal and a vertical ramp, so a rotation shows.
fn base_rgb() -> Vec<u8> {
    let mut rgb = Vec::with_capacity((WIDTH * HEIGHT * 3) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            rgb.extend_from_slice(&[40 + 2 * x as u8, 60 + 2 * y as u8, 120]);
        }
    }
    rgb
}

/// A map that's brightest in the top-left corner and ramps away from it, so a map that isn't
/// rotated with its base lands visibly wrong.
fn map_value(x: u32, y: u32) -> u8 {
    (255 - (x * 255 / (MAP_WIDTH - 1)) / 2 - (y * 255 / (MAP_HEIGHT - 1)) / 2) as u8
}

fn encode(pixels: &[u8], width: u32, height: u32, color: ExtendedColorType) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 95)
        .encode(pixels, width, height, color)
        .expect("encode JPEG");
    out
}

/// An APPn segment: marker, big-endian length (counting itself), payload.
fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let len = u16::try_from(payload.len() + 2).expect("segment fits");
    let mut out = vec![0xFF, marker];
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn xmp_segment(packet: &str) -> Vec<u8> {
    let mut payload = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    payload.extend_from_slice(packet.as_bytes());
    segment(0xE1, &payload)
}

fn exif_segment(tiff: &[u8]) -> Vec<u8> {
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(tiff);
    segment(0xE1, &payload)
}

const ISO_URN: &[u8] = b"urn:iso:std:iso:ts:21496:-1\0";

fn iso_segment(body: &[u8]) -> Vec<u8> {
    let mut payload = ISO_URN.to_vec();
    payload.extend_from_slice(body);
    segment(0xE2, &payload)
}

fn icc_segment(icc: &[u8]) -> Vec<u8> {
    let mut payload = b"ICC_PROFILE\0".to_vec();
    payload.extend_from_slice(&[1, 1]);
    payload.extend_from_slice(icc);
    segment(0xE2, &payload)
}

/// `jpeg` with `segments` inserted right after its SOI marker.
fn with_segments(jpeg: &[u8], segments: &[Vec<u8>]) -> Vec<u8> {
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    let mut out = jpeg[..2].to_vec();
    for s in segments {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// Big-endian MP index: primary at offset 0, the map at `map_offset` from the TIFF header.
fn mpf_payload(primary_len: u32, map_len: u32, map_offset: u32) -> Vec<u8> {
    let mut out = b"MPF\0MM\x00\x2a".to_vec();
    out.extend_from_slice(&8u32.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
    let entry = |out: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: [u8; 4]| {
        out.extend_from_slice(&tag.to_be_bytes());
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(&value);
    };
    entry(&mut out, 0xB000, 7, 4, *b"0100");
    entry(&mut out, 0xB001, 4, 1, 2u32.to_be_bytes());
    // The entry table follows the IFD: 8 header + 2 count + 3 × 12 + 4 next-IFD.
    entry(&mut out, 0xB002, 7, 32, 50u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    for (attribute, size, offset) in [
        (0x0003_0000u32, primary_len, 0u32),
        (0, map_len, map_offset),
    ] {
        out.extend_from_slice(&attribute.to_be_bytes());
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&[0; 4]);
    }
    out
}

/// The primary (its own segments plus an MPF index, last) followed by the map.
fn assemble(base_jpeg: &[u8], primary_segments: Vec<Vec<u8>>, map_jpeg: &[u8]) -> Vec<u8> {
    let build = |primary_len: u32, map_offset: u32| {
        let mut segments = primary_segments.clone();
        segments.push(segment(
            0xE2,
            &mpf_payload(primary_len, map_jpeg.len() as u32, map_offset),
        ));
        with_segments(base_jpeg, &segments)
    };
    // The index's size doesn't depend on its values, so measure once and fill them in.
    let draft = build(0, 0);
    let tiff_at = 2 + primary_segments.iter().map(Vec::len).sum::<usize>() + 4 + 4;
    let primary_len = draft.len() as u32;
    let mut file = build(primary_len, primary_len - tiff_at as u32);
    assert_eq!(file.len(), draft.len());
    file.extend_from_slice(map_jpeg);
    file
}

/// EXIF holding only an orientation.
fn orientation_exif(orientation: u16) -> Vec<u8> {
    let mut tiff = b"II\x2a\x00".to_vec();
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff
}

fn single_channel_map() -> Vec<u8> {
    let mut map = Vec::new();
    for y in 0..MAP_HEIGHT {
        for x in 0..MAP_WIDTH {
            map.push(map_value(x, y));
        }
    }
    map
}

fn ultra_hdr(orientation: Option<u16>) -> Vec<u8> {
    let map_xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
   hdrgm:Version="1.0"
   hdrgm:GainMapMin="0.0"
   hdrgm:GainMapMax="2.0"
   hdrgm:Gamma="1.0"
   hdrgm:OffsetSDR="0.015625"
   hdrgm:OffsetHDR="0.015625"
   hdrgm:HDRCapacityMin="0.0"
   hdrgm:HDRCapacityMax="2.0"
   hdrgm:BaseRenditionIsHDR="False"/>
 </rdf:RDF>
</x:xmpmeta>"#;
    let map = with_segments(
        &encode(
            &single_channel_map(),
            MAP_WIDTH,
            MAP_HEIGHT,
            ExtendedColorType::L8,
        ),
        &[xmp_segment(map_xmp)],
    );
    let primary_xmp = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
        xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
        xmlns:Container="http://ns.google.com/photos/1.0/container/"
        xmlns:Item="http://ns.google.com/photos/1.0/container/item/"
      hdrgm:Version="1.0">
      <Container:Directory>
        <rdf:Seq>
          <rdf:li rdf:parseType="Resource">
            <Container:Item Item:Semantic="Primary" Item:Mime="image/jpeg"/>
          </rdf:li>
          <rdf:li rdf:parseType="Resource">
            <Container:Item Item:Semantic="GainMap" Item:Mime="image/jpeg" Item:Length="{}"/>
          </rdf:li>
        </rdf:Seq>
      </Container:Directory>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>"#,
        map.len()
    );
    let mut segments = Vec::new();
    if let Some(o) = orientation {
        segments.push(exif_segment(&orientation_exif(o)));
    }
    segments.push(xmp_segment(&primary_xmp));
    let base = encode(&base_rgb(), WIDTH, HEIGHT, ExtendedColorType::Rgb8);
    assemble(&base, segments, &map)
}

fn iso_21496() -> Vec<u8> {
    let mut rgb_map = Vec::new();
    for y in 0..MAP_HEIGHT {
        for x in 0..MAP_WIDTH {
            rgb_map.extend_from_slice(&[map_value(x, y), (x * 17) as u8, (y * 23) as u8]);
        }
    }
    let mut m = ultrahdr_core::GainMapMetadata::default();
    for (c, (min, max, gamma)) in [(0.0, 2.0, 1.0), (-0.5, 1.5, 0.8), (0.0, 1.0, 1.2)]
        .into_iter()
        .enumerate()
    {
        m.channels[c].min = min;
        m.channels[c].max = max;
        m.channels[c].gamma = gamma;
        m.channels[c].base_offset = 1.0 / 64.0;
        m.channels[c].alternate_offset = 1.0 / 64.0;
    }
    m.base_hdr_headroom = 0.0;
    m.alternate_hdr_headroom = 2.0;
    let params = ultrahdr_core::serialize_iso21496_fmt(&m, ultrahdr_core::Iso21496Format::JxlJhgm);
    let map = with_segments(
        &encode(&rgb_map, MAP_WIDTH, MAP_HEIGHT, ExtendedColorType::Rgb8),
        &[iso_segment(&params)],
    );
    let p3 = moxcms::ColorProfile::new_display_p3()
        .encode()
        .expect("P3 profile");
    let base = encode(&base_rgb(), WIDTH, HEIGHT, ExtendedColorType::Rgb8);
    // The primary's ISO segment is the version alone: "this file has an ISO gain map".
    assemble(
        &base,
        vec![icc_segment(&p3), iso_segment(&[0, 0, 0, 0])],
        &map,
    )
}

/// EXIF with an Apple maker note: `HDRHeadroom` (0x21) 1.01 and `HDRGain` (0x30) 0.5, both
/// signed rationals, in the custom IFD layout iOS writes (entry count right after the byte-order
/// marker, out-of-line values relative to the maker note's start).
fn apple_exif() -> Vec<u8> {
    let mut maker = b"Apple iOS\0\x00\x01MM".to_vec();
    maker.extend_from_slice(&2u16.to_be_bytes());
    let values_at = maker.len() as u32 + 2 * 12 + 4;
    for (i, tag) in [0x0021u16, 0x0030].into_iter().enumerate() {
        maker.extend_from_slice(&tag.to_be_bytes());
        maker.extend_from_slice(&10u16.to_be_bytes());
        maker.extend_from_slice(&1u32.to_be_bytes());
        maker.extend_from_slice(&(values_at + 8 * i as u32).to_be_bytes());
    }
    maker.extend_from_slice(&0u32.to_be_bytes());
    for (num, den) in [(101i32, 100i32), (1, 2)] {
        maker.extend_from_slice(&num.to_be_bytes());
        maker.extend_from_slice(&den.to_be_bytes());
    }

    // IFD0 → Exif IFD → MakerNote, big-endian like an iPhone.
    let mut tiff = b"MM\x00\x2a".to_vec();
    tiff.extend_from_slice(&8u32.to_be_bytes());
    // IFD0 at 8: one entry, the Exif IFD pointer.
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x8769u16.to_be_bytes());
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    let exif_ifd = 8 + 2 + 12 + 4;
    tiff.extend_from_slice(&(exif_ifd as u32).to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());
    // Exif IFD: one entry, the maker note, stored right after it.
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x927Cu16.to_be_bytes());
    tiff.extend_from_slice(&7u16.to_be_bytes());
    tiff.extend_from_slice(&(maker.len() as u32).to_be_bytes());
    tiff.extend_from_slice(&((exif_ifd + 2 + 12 + 4) as u32).to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());
    tiff.extend_from_slice(&maker);
    tiff
}

fn apple() -> Vec<u8> {
    let map_xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
   <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about=""
            xmlns:apdi="http://ns.apple.com/pixeldatainfo/1.0/">
         <apdi:AuxiliaryImageType>urn:com:apple:photo:2020:aux:hdrgainmap</apdi:AuxiliaryImageType>
      </rdf:Description>
   </rdf:RDF>
</x:xmpmeta>"#;
    let map = with_segments(
        &encode(
            &single_channel_map(),
            MAP_WIDTH,
            MAP_HEIGHT,
            ExtendedColorType::L8,
        ),
        &[xmp_segment(map_xmp)],
    );
    let base = encode(&base_rgb(), WIDTH, HEIGHT, ExtendedColorType::Rgb8);
    assemble(&base, vec![exif_segment(&apple_exif())], &map)
}
