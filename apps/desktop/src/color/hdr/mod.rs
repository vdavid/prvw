//! HDR stills beyond RAW: gain-map JPEGs, PQ/HLG PNGs, and float TIFFs. The plan is
//! `docs/specs/hdr-images.md`.
//!
//! `ultrahdr-core` is pre-1.0 and renames things between minor versions, so the rest of the app
//! reaches it only through this module, and a bump touches one place. It's added with
//! `default-features = false` on purpose: see the comment on it in `apps/desktop/Cargo.toml`.
//!
//! - `fragment` — the Rust reference for the image shader's colour math, per `ShaderMode`.
//! - `gain_map` — gain-map parameters in one shape whatever metadata they came from, and the
//!   math mode 2 runs on them.
//!
//! The metadata parsers here are the two `ultrahdr-core` still has under a permissive license:
//! ISO 21496-1 and Apple's maker note. Ultra HDR's XMP and the MPF index are ours, in
//! `decoding::gain_map`.

pub mod fragment;
pub mod gain_map;
// Nothing outside the tests calls these until the decoders and shader mode 3 land.
#[allow(dead_code)]
pub mod tone_map;
#[allow(dead_code)]
pub mod transfer;

use gain_map::{ChannelParams, GainMapParams};

/// Read ISO 21496-1 gain-map parameters from an APP2 payload, the bytes after the
/// `urn:iso:std:iso:ts:21496:-1\0` identifier. `None` for the four-byte "version only" form the
/// primary image carries, and for anything malformed or unusable.
pub fn parse_iso21496(payload: &[u8]) -> Option<GainMapParams> {
    match ultrahdr_core::parse_iso21496_fmt(payload, ultrahdr_core::Iso21496Format::JxlJhgm) {
        Ok(m) => from_upstream(&m),
        Err(e) => {
            log::debug!("ISO 21496-1 gain-map metadata didn't parse: {e:?}");
            None
        }
    }
}

/// Read Apple's gain-map parameters from a primary image's EXIF (the APP1 payload after
/// `Exif\0\0`): the maker note's headroom tags, converted the way `ultrahdr-core` documents.
/// `None` when there's no Apple maker note or no headroom tag in it.
pub fn parse_apple(exif: &[u8]) -> Option<GainMapParams> {
    let info = ultrahdr_core::parse_exif_for_apple_hdr(exif)?;
    from_upstream(&ultrahdr_core::from_apple_headroom(&info)?)
}

fn from_upstream(m: &ultrahdr_core::GainMapMetadata) -> Option<GainMapParams> {
    let params = GainMapParams {
        channels: m.channels.map(|c| ChannelParams {
            min: c.min as f32,
            max: c.max as f32,
            gamma: c.gamma as f32,
            base_offset: c.base_offset as f32,
            alternate_offset: c.alternate_offset as f32,
        }),
        base_headroom: m.base_hdr_headroom as f32,
        alternate_headroom: m.alternate_hdr_headroom as f32,
        use_base_color_space: m.use_base_color_space,
    };
    params.is_usable().then_some(params)
}

/// Our parameters back in `ultrahdr-core`'s shape, for tests that compare against it.
#[cfg(test)]
pub(crate) fn to_upstream(p: &GainMapParams) -> ultrahdr_core::GainMapMetadata {
    let mut m = ultrahdr_core::GainMapMetadata::default();
    for (dst, src) in m.channels.iter_mut().zip(p.channels) {
        dst.min = f64::from(src.min);
        dst.max = f64::from(src.max);
        dst.gamma = f64::from(src.gamma);
        dst.base_offset = f64::from(src.base_offset);
        dst.alternate_offset = f64::from(src.alternate_offset);
    }
    m.base_hdr_headroom = f64::from(p.base_headroom);
    m.alternate_hdr_headroom = f64::from(p.alternate_headroom);
    m.use_base_color_space = p.use_base_color_space;
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use ultrahdr_core::GainMapMetadata;
    use ultrahdr_core::gainmap::apply::calculate_weight;

    /// A round trip through upstream's own serializer: the reader is the one we wrap, so this
    /// pins the field mapping (log2 gains, linear offsets, headrooms), not the wire format.
    #[test]
    fn iso_payload_round_trips_per_channel() {
        let mut m = GainMapMetadata::default();
        for (c, ch) in m.channels.iter_mut().enumerate() {
            ch.min = -0.25 * c as f64;
            ch.max = 2.0 + c as f64 * 0.5;
            ch.gamma = 1.0 + c as f64 * 0.1;
            ch.base_offset = 0.01;
            ch.alternate_offset = 0.02;
        }
        m.base_hdr_headroom = 0.0;
        m.alternate_hdr_headroom = 2.5;
        let payload =
            ultrahdr_core::serialize_iso21496_fmt(&m, ultrahdr_core::Iso21496Format::JxlJhgm);
        let p = parse_iso21496(&payload).expect("round trip");
        assert_eq!(p.channels.map(|c| c.max), [2.0, 2.5, 3.0]);
        assert!((p.channels[2].gamma - 1.2).abs() < 1e-6);
        assert!((p.channels[1].base_offset - 0.01).abs() < 1e-6);
        assert_eq!(p.alternate_headroom, 2.5);
        assert!(p.use_base_color_space);
        assert_eq!(from_upstream(&to_upstream(&p)), Some(p));
    }

    /// The primary image's ISO segment is four version bytes and no parameters.
    #[test]
    fn the_version_only_iso_segment_is_no_metadata() {
        assert_eq!(parse_iso21496(&[0, 0, 0, 0]), None);
        assert_eq!(parse_iso21496(&[]), None);
    }

    /// Upstream's reader on hostile bytes: every truncation answers, none panics.
    #[test]
    fn truncated_iso_payloads_never_panic() {
        let payload = ultrahdr_core::serialize_iso21496_fmt(
            &GainMapMetadata::default(),
            ultrahdr_core::Iso21496Format::JxlJhgm,
        );
        for len in 0..payload.len() {
            let _ = parse_iso21496(&payload[..len]);
        }
    }

    #[test]
    fn exif_without_an_apple_maker_note_is_no_metadata() {
        assert_eq!(parse_apple(b"II\x2a\x00\x08\x00\x00\x00\x00\x00"), None);
        assert_eq!(parse_apple(&[]), None);
    }

    fn metadata(base_headroom: f64, alternate_headroom: f64) -> GainMapMetadata {
        let mut m = GainMapMetadata::default();
        m.base_hdr_headroom = base_headroom;
        m.alternate_hdr_headroom = alternate_headroom;
        m
    }

    /// The usual direction (SDR base, HDR alternate) is the one `calculate_weight` gets right,
    /// so our shader port can be tested against it there.
    #[test]
    fn upstream_weight_matches_the_formula_for_an_sdr_base() {
        let m = metadata(0.0, 2.0);
        assert_eq!(calculate_weight(1.0, &m), 0.0);
        assert_eq!(calculate_weight(2.0, &m), 0.5);
        assert_eq!(calculate_weight(4.0, &m), 1.0);
        assert_eq!(calculate_weight(16.0, &m), 1.0);
    }

    /// Regression anchor for a finding in the spec (2026-10-13): with an HDR base (ISO 21496-1
    /// allows it), `ultrahdr-core` 0.6.0 returns a weight of 1.0 at every headroom, which would
    /// show the SDR rendition even on a display that can show the HDR base as is. The correct
    /// weight there runs from 1.0 at headroom 1 down to 0.0 at the base's own headroom, so the
    /// gain-map port takes libavif's formula for that direction. If an upgrade makes this test
    /// fail, upstream changed the behavior: re-check the port against it.
    #[test]
    fn upstream_weight_ignores_an_hdr_base() {
        let m = metadata(2.0, 0.0);
        for headroom in [1.0, 2.0, 4.0, 16.0] {
            assert_eq!(calculate_weight(headroom, &m), 1.0);
        }
    }
}
