//! Read a [`Dcp`] straight out of the profile tags embedded in a DNG.
//!
//! Standalone `.dcp` files are TIFF-IFD-ish containers with an `IIRC` magic.
//! DNG files carry the exact same profile tags in their main IFD — that's
//! the whole point of the DNG spec's § 6.2 "Camera Profile" section, and
//! it's why Pixel, Samsung Galaxy, and iPhone ProRAW photos don't ship an
//! external profile: their camera manufacturer already stashed one in the
//! file.
//!
//! We reuse the same [`Dcp`] / [`HueSatMap`] types that [`parser::parse`]
//! produces for standalone files, so the downstream `apply_hue_sat_map`
//! code doesn't know (or care) where the profile came from.
//!
//! The input map is `HashMap<u16, Value>` — the shape rawler uses for
//! `RawImage::dng_tags` and what you get from
//! `decoder.ifd(WellKnownIFD::VirtualDngRootTags|Root)`. `Value` is rawler's
//! typed, already-endian-normalised TIFF value enum, so no byte-swapping
//! work happens here.
//!
//! ## What we read
//!
//! | Tag | Name                          | Required for a match? |
//! |-----|-------------------------------|------------------------|
//! | 50708 | `UniqueCameraModel`         | No, logged only |
//! | 50778 | `CalibrationIlluminant1`    | No |
//! | 50779 | `CalibrationIlluminant2`    | No |
//! | 50932 | `ProfileCalibrationSignature` | No |
//! | 50936 | `ProfileName`               | No |
//! | 50937 | `ProfileHueSatMapDims`      | **Yes** |
//! | 50938 | `ProfileHueSatMapData1`     | **Yes** (or `Data2`) |
//! | 50939 | `ProfileHueSatMapData2`     | **Yes** (or `Data1`) |
//! | 50942 | `ProfileCopyright`          | No, logged only |
//! | 51107 | `ProfileHueSatMapEncoding`  | No, defaults to `0` (linear) |
//!
//! Tags we intentionally skip for now (Phase 3.3 scope): `ForwardMatrix1/2`,
//! `CameraCalibration1/2`, `ProfileToneCurve`, `ProfileLookTable*`. These are
//! tracked in the Phase 3.x roadmap; applying them all at once risks scope
//! creep and double-correction with the work earlier pipeline stages
//! already do.
//!
//! ## Return value
//!
//! `None` when the DNG carries no usable profile (no dims, no data, dims
//! imply one map size but the data length says another, etc.). Callers
//! treat `None` as "fall back to filesystem DCP discovery, then to the
//! default pipeline" — the pre-3.3 behaviour.

use std::collections::HashMap;

use rawler::formats::tiff::Value;

use super::parser::{Dcp, HueSatMap};

// Tag numeric constants. We keep the same literal values parser.rs uses so
// the two code paths stay byte-for-byte aligned against the DNG 1.6 spec.
const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_CALIBRATION_ILLUMINANT_1: u16 = 50778;
const TAG_CALIBRATION_ILLUMINANT_2: u16 = 50779;
const TAG_PROFILE_CALIBRATION_SIGNATURE: u16 = 50932;
const TAG_PROFILE_NAME: u16 = 50936;
const TAG_PROFILE_HUE_SAT_MAP_DIMS: u16 = 50937;
const TAG_PROFILE_HUE_SAT_MAP_DATA_1: u16 = 50938;
const TAG_PROFILE_HUE_SAT_MAP_DATA_2: u16 = 50939;
const TAG_PROFILE_COPYRIGHT: u16 = 50942;
const TAG_PROFILE_HUE_SAT_MAP_ENCODING: u16 = 51107;

/// Build a [`Dcp`] from a map of DNG tags. Returns `None` when the DNG
/// carries no applicable profile (missing dims, missing both hue/sat map
/// payloads, or a size mismatch between them).
pub fn from_dng_tags(tags: &HashMap<u16, Value>) -> Option<Dcp> {
    let dims = dims_from_tag(tags.get(&TAG_PROFILE_HUE_SAT_MAP_DIMS)?)?;
    let [hue_divs, sat_divs, val_divs] = dims;
    let expected_floats = (hue_divs as usize)
        .checked_mul(sat_divs as usize)?
        .checked_mul(val_divs as usize)?
        .checked_mul(3)?;

    let map_1 = tags
        .get(&TAG_PROFILE_HUE_SAT_MAP_DATA_1)
        .and_then(|v| floats_from_tag(v, expected_floats))
        .map(|data| HueSatMap {
            hue_divs,
            sat_divs,
            val_divs,
            data,
        });
    let map_2 = tags
        .get(&TAG_PROFILE_HUE_SAT_MAP_DATA_2)
        .and_then(|v| floats_from_tag(v, expected_floats))
        .map(|data| HueSatMap {
            hue_divs,
            sat_divs,
            val_divs,
            data,
        });
    if map_1.is_none() && map_2.is_none() {
        return None;
    }

    let encoding = tags
        .get(&TAG_PROFILE_HUE_SAT_MAP_ENCODING)
        .and_then(u32_from_tag)
        .unwrap_or(0);

    Some(Dcp {
        unique_camera_model: tags.get(&TAG_UNIQUE_CAMERA_MODEL).and_then(string_from_tag),
        profile_name: tags.get(&TAG_PROFILE_NAME).and_then(string_from_tag),
        profile_copyright: tags.get(&TAG_PROFILE_COPYRIGHT).and_then(string_from_tag),
        profile_calibration_signature: tags
            .get(&TAG_PROFILE_CALIBRATION_SIGNATURE)
            .and_then(string_from_tag),
        calibration_illuminant_1: tags
            .get(&TAG_CALIBRATION_ILLUMINANT_1)
            .and_then(u16_from_tag),
        calibration_illuminant_2: tags
            .get(&TAG_CALIBRATION_ILLUMINANT_2)
            .and_then(u16_from_tag),
        hue_sat_map_1: map_1,
        hue_sat_map_2: map_2,
        hue_sat_map_encoding: encoding,
    })
}

/// Read a `ProfileHueSatMapDims` value: three `LONG`s for hue, sat, val
/// divisions. `Short` is accepted too as a best-effort read — the spec
/// calls for `LONG`, but a lenient reader is cheaper than a refusal.
fn dims_from_tag(value: &Value) -> Option<[u32; 3]> {
    match value {
        Value::Long(v) if v.len() >= 3 => Some([v[0], v[1], v[2]]),
        Value::Short(v) if v.len() >= 3 => Some([v[0] as u32, v[1] as u32, v[2] as u32]),
        _ => None,
    }
}

/// Read a hue/sat map payload: `expected_floats` × f32, packed as the
/// spec's `(hue_shift_degrees, sat_scale, val_scale)` triples. Only
/// `Float` is spec-conformant, but we also accept `Double` in case a
/// writer used a wider type (reality is kinder than the spec sometimes).
fn floats_from_tag(value: &Value, expected_floats: usize) -> Option<Vec<f32>> {
    match value {
        Value::Float(v) if v.len() == expected_floats => Some(v.clone()),
        Value::Double(v) if v.len() == expected_floats => {
            Some(v.iter().map(|&x| x as f32).collect())
        }
        _ => None,
    }
}

/// Read the first element of an integer-ish tag as `u32`. Covers the
/// types writers commonly pick for `ProfileHueSatMapEncoding`.
fn u32_from_tag(value: &Value) -> Option<u32> {
    match value {
        Value::Long(v) => v.first().copied(),
        Value::Short(v) => v.first().map(|&x| x as u32),
        Value::Byte(v) => v.first().map(|&x| x as u32),
        _ => None,
    }
}

/// Read the first element of a `SHORT` tag as `u16`. Used for the two
/// calibration illuminant tags.
fn u16_from_tag(value: &Value) -> Option<u16> {
    match value {
        Value::Short(v) => v.first().copied(),
        Value::Long(v) => v.first().map(|&x| x as u16),
        _ => None,
    }
}

/// Read an ASCII tag as an owned `String`. Strips any trailing NUL and
/// surrounding whitespace to match `parser.rs`.
fn string_from_tag(value: &Value) -> Option<String> {
    match value {
        Value::Ascii(s) => {
            let raw = s.strings().first()?.clone();
            let trimmed = raw.trim_end_matches('\0').trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawler::formats::tiff::TiffAscii;

    fn identity_map_data(hue_divs: u32, sat_divs: u32, val_divs: u32) -> Vec<f32> {
        let entries = (hue_divs * sat_divs * val_divs) as usize;
        let mut data = Vec::with_capacity(entries * 3);
        for _ in 0..entries {
            data.push(0.0); // hue shift degrees
            data.push(1.0); // sat scale
            data.push(1.0); // val scale
        }
        data
    }

    /// Insert the minimum viable profile (dims + one data map) into a
    /// fresh tag map. Handy for the happy-path tests below.
    fn minimal_tag_map(hue_divs: u32, sat_divs: u32, val_divs: u32) -> HashMap<u16, Value> {
        let mut map = HashMap::new();
        map.insert(
            TAG_PROFILE_HUE_SAT_MAP_DIMS,
            Value::Long(vec![hue_divs, sat_divs, val_divs]),
        );
        map.insert(
            TAG_PROFILE_HUE_SAT_MAP_DATA_1,
            Value::Float(identity_map_data(hue_divs, sat_divs, val_divs)),
        );
        map
    }

    #[test]
    fn returns_none_when_dims_missing() {
        let mut map = HashMap::new();
        map.insert(
            TAG_PROFILE_HUE_SAT_MAP_DATA_1,
            Value::Float(vec![0.0, 1.0, 1.0]),
        );
        assert!(from_dng_tags(&map).is_none());
    }

    #[test]
    fn returns_none_when_both_data_maps_missing() {
        let mut map = HashMap::new();
        map.insert(TAG_PROFILE_HUE_SAT_MAP_DIMS, Value::Long(vec![2, 2, 1]));
        assert!(from_dng_tags(&map).is_none());
    }

    #[test]
    fn reads_minimal_profile_from_data_1() {
        let tags = minimal_tag_map(2, 2, 1);
        let dcp = from_dng_tags(&tags).expect("should parse minimal profile");
        assert!(dcp.hue_sat_map_1.is_some(), "data_1 should be populated");
        assert!(dcp.hue_sat_map_2.is_none());
        let map = dcp.hue_sat_map_1.unwrap();
        assert_eq!((map.hue_divs, map.sat_divs, map.val_divs), (2, 2, 1));
        assert_eq!(map.data.len(), 2 * 2 * 3);
        assert_eq!(dcp.hue_sat_map_encoding, 0, "defaults to linear encoding");
    }

    #[test]
    fn reads_only_data_2_when_data_1_missing() {
        // Some DNGs ship only the D65 slot in `Data2`; make sure the
        // single-slot case works either way.
        let mut tags = HashMap::new();
        tags.insert(TAG_PROFILE_HUE_SAT_MAP_DIMS, Value::Long(vec![2, 2, 1]));
        tags.insert(
            TAG_PROFILE_HUE_SAT_MAP_DATA_2,
            Value::Float(identity_map_data(2, 2, 1)),
        );
        let dcp = from_dng_tags(&tags).expect("should parse profile with only Data2");
        assert!(dcp.hue_sat_map_1.is_none());
        assert!(dcp.hue_sat_map_2.is_some());
    }

    #[test]
    fn missing_illuminant_leaves_it_none_but_still_parses() {
        let tags = minimal_tag_map(2, 2, 1);
        let dcp = from_dng_tags(&tags).expect("should still produce a Dcp");
        assert!(dcp.calibration_illuminant_1.is_none());
        assert!(dcp.calibration_illuminant_2.is_none());
        // `pick_hue_sat_map` must still return something — it defers to
        // the first populated map when no D65 illuminant is specified.
        assert!(dcp.pick_hue_sat_map().is_some());
    }

    #[test]
    fn reads_full_metadata_including_strings() {
        let mut tags = minimal_tag_map(2, 2, 1);
        tags.insert(
            TAG_UNIQUE_CAMERA_MODEL,
            Value::Ascii(TiffAscii::new("Google Pixel 6 Pro")),
        );
        tags.insert(
            TAG_PROFILE_NAME,
            Value::Ascii(TiffAscii::new("Google Embedded Camera Profile")),
        );
        tags.insert(
            TAG_PROFILE_COPYRIGHT,
            Value::Ascii(TiffAscii::new("Copyright 2021 Google LLC")),
        );
        tags.insert(TAG_CALIBRATION_ILLUMINANT_1, Value::Short(vec![17]));
        tags.insert(TAG_CALIBRATION_ILLUMINANT_2, Value::Short(vec![21]));
        tags.insert(TAG_PROFILE_HUE_SAT_MAP_ENCODING, Value::Long(vec![1]));
        // Add Data2 too so the pick-map logic has a choice.
        tags.insert(
            TAG_PROFILE_HUE_SAT_MAP_DATA_2,
            Value::Float(identity_map_data(2, 2, 1)),
        );

        let dcp = from_dng_tags(&tags).expect("should parse");
        assert_eq!(
            dcp.unique_camera_model.as_deref(),
            Some("Google Pixel 6 Pro")
        );
        assert_eq!(
            dcp.profile_name.as_deref(),
            Some("Google Embedded Camera Profile")
        );
        assert_eq!(
            dcp.profile_copyright.as_deref(),
            Some("Copyright 2021 Google LLC")
        );
        assert_eq!(dcp.calibration_illuminant_1, Some(17));
        assert_eq!(dcp.calibration_illuminant_2, Some(21));
        assert_eq!(dcp.hue_sat_map_encoding, 1);
        // With illuminant 2 = D65, pick_hue_sat_map prefers map 2.
        let picked = dcp.pick_hue_sat_map().expect("picked");
        assert!(std::ptr::eq(picked, dcp.hue_sat_map_2.as_ref().unwrap()));
    }

    #[test]
    fn rejects_size_mismatch_between_dims_and_data() {
        // dims say 2×2×1 → 12 floats; supply only 3 → parse declines.
        let mut tags = HashMap::new();
        tags.insert(TAG_PROFILE_HUE_SAT_MAP_DIMS, Value::Long(vec![2, 2, 1]));
        tags.insert(
            TAG_PROFILE_HUE_SAT_MAP_DATA_1,
            Value::Float(vec![0.0, 1.0, 1.0]),
        );
        assert!(from_dng_tags(&tags).is_none());
    }

    #[test]
    fn accepts_double_payload_as_fallback() {
        // Spec calls for f32 but a liberal reader accepts f64 payloads too.
        let mut tags = HashMap::new();
        tags.insert(TAG_PROFILE_HUE_SAT_MAP_DIMS, Value::Long(vec![2, 2, 1]));
        let doubles: Vec<f64> = identity_map_data(2, 2, 1)
            .iter()
            .map(|&f| f as f64)
            .collect();
        tags.insert(TAG_PROFILE_HUE_SAT_MAP_DATA_1, Value::Double(doubles));
        let dcp = from_dng_tags(&tags).expect("f64 payload should be accepted");
        assert_eq!(dcp.hue_sat_map_1.as_ref().unwrap().data.len(), 12);
    }
}
