//! Adobe DCP (Digital Camera Profile) binary parser.
//!
//! A DCP file is a TIFF-like container with an 8-byte header followed by a
//! standard TIFF IFD:
//!
//! ```text
//!   bytes 0–3  : magic `IIRC` (little-endian, we reject anything else)
//!   bytes 4–7  : u32 LE offset to the first IFD (always 8 in practice)
//!   bytes 8..  : TIFF IFD (u16 entry count, N × 12-byte entries, u32 next-IFD ptr)
//! ```
//!
//! DCPs are always little-endian, per Adobe's spec. Big-endian `MMRC` is not
//! specified anywhere. We refuse to guess.
//!
//! We hand-roll the parser rather than routing through `rawler::formats::tiff`
//! because DCP's magic isn't `II\x2a\x00`, so `GenericTiffReader` would reject
//! it at the front door. The parser only decodes the subset of DNG tags that
//! are relevant to per-camera color refinement; other tags are ignored.
//!
//! # References
//!
//! - DNG 1.6 spec, § 6.2 "Camera Profile" — canonical reference.
//! - Tags extracted match the `DngTag` IDs rawler already knows about:
//!   `UniqueCameraModel` (50708), `ProfileCalibrationSignature` (50932),
//!   `CalibrationIlluminant1/2` (50778/50779), `ProfileName` (50936),
//!   `ProfileHueSatMapDims` (50937), `ProfileHueSatMapData1/2` (50938/50939),
//!   `ProfileHueSatMapEncoding` (51107), `ProfileToneCurve` (50940),
//!   `ProfileCopyright` (50942).

use std::fmt;

/// Parsed DCP. Only the fields we actually consume are present; every field
/// is optional because real-world DCPs vary in what they ship.
#[derive(Debug, Clone)]
pub struct Dcp {
    /// `UniqueCameraModel` tag. Adobe's canonical form is `"<Make> <Model>"`
    /// (e.g., `"Sony ILCE-7M3"`). Used for matching.
    pub unique_camera_model: Option<String>,
    /// Human-readable profile name (e.g., `"Adobe Standard"` or `"SONY
    /// ILCE-7M3"`). Not used for matching; logged for user visibility.
    pub profile_name: Option<String>,
    /// Copyright / attribution string. Logged at debug level.
    pub profile_copyright: Option<String>,
    /// `ProfileCalibrationSignature` — optional alternative matching key.
    /// When set, matches any RAW carrying the same signature.
    pub profile_calibration_signature: Option<String>,
    /// EXIF `LightSource` code for illuminant 1 of the hue/sat map.
    /// `17` = Standard Light A (~2856 K), `21` = D65 (~6504 K), etc.
    pub calibration_illuminant_1: Option<u16>,
    /// EXIF `LightSource` code for illuminant 2. Optional.
    pub calibration_illuminant_2: Option<u16>,
    /// 3D hue/sat map for illuminant 1. See [`HueSatMap`].
    pub hue_sat_map_1: Option<HueSatMap>,
    /// 3D hue/sat map for illuminant 2. Optional.
    pub hue_sat_map_2: Option<HueSatMap>,
    /// Encoding flag for the hue/sat map values. `0` = linear (default), `1`
    /// = sRGB gamma. Affects how the value-division axis is interpreted.
    pub hue_sat_map_encoding: u32,
    /// Optional `ProfileLookTable` — a second 3D HSV LUT applied **after**
    /// the HueSatMap per DNG 1.6 § 6.2.3. Same shape and math as the hue/sat
    /// map; captures Adobe's "Look" refinement on top of the camera's
    /// neutral calibration. Single-illuminant (no per-illuminant variant),
    /// so one map for the whole profile.
    pub look_table: Option<HueSatMap>,
    /// Encoding flag for the look table's value axis. `0` = linear
    /// (default), `1` = sRGB gamma. Same semantics as `hue_sat_map_encoding`.
    pub look_table_encoding: u32,
    /// Optional `ProfileToneCurve` (tag 50940) — a list of `(x, y)` float
    /// pairs defining a per-camera tone curve the profile author prefers
    /// over a generic one. Monotonically increasing on both axes, endpoints
    /// at (0, 0) and (1, 1) by spec. When present, Prvw applies it
    /// **instead of** our default tone curve so the camera's intended
    /// tonality wins.
    pub tone_curve: Option<Vec<(f32, f32)>>,
}

impl Dcp {
    /// Pick the hue/sat map to apply. Preference order: the D65 map (22 per
    /// EXIF + 21 for plain D65, but Adobe's actual code uses 21), then the
    /// first map that exists. Falls back to `hue_sat_map_1` when there's no
    /// second one.
    ///
    /// This is intentionally conservative: color-temperature-aware
    /// interpolation between illuminants 1 and 2 is a nice-to-have we've
    /// deferred for a later phase. Sticking to D65 keeps the color math
    /// aligned with our D65 camera-matrix assumption in `raw.rs`.
    pub fn pick_hue_sat_map(&self) -> Option<&HueSatMap> {
        // LightSource 21 = D65. The Adobe convention is to put the "cool"
        // illuminant on slot 2 when two are provided; check that one first.
        if self.calibration_illuminant_2 == Some(21)
            && let Some(ref m) = self.hue_sat_map_2
        {
            return Some(m);
        }
        if self.calibration_illuminant_1 == Some(21)
            && let Some(ref m) = self.hue_sat_map_1
        {
            return Some(m);
        }
        self.hue_sat_map_1.as_ref().or(self.hue_sat_map_2.as_ref())
    }
}

/// A 3D hue/sat map. The spec defines it as a grid of
/// `hue_divs × sat_divs × val_divs` RGB-f32 triples where each triple is
/// `(hue_shift_degrees, sat_scale, val_scale)`.
#[derive(Debug, Clone)]
pub struct HueSatMap {
    pub hue_divs: u32,
    pub sat_divs: u32,
    pub val_divs: u32,
    /// Flat `Vec<f32>` of length `hue_divs * sat_divs * val_divs * 3`, layout
    /// is hue-outermost, val-innermost, matching the DNG spec's encoding.
    /// Each entry is `(hue_shift_deg, sat_scale, val_scale)`.
    pub data: Vec<f32>,
}

impl HueSatMap {
    /// Sample the map at grid coordinates `(h_idx, s_idx, v_idx)`. No
    /// bounds-checking — callers must clamp.
    pub fn sample(&self, h_idx: u32, s_idx: u32, v_idx: u32) -> (f32, f32, f32) {
        let sat = self.sat_divs;
        let val = self.val_divs;
        // Index layout per DNG spec 1.6 § 6.2 "ProfileHueSatMapData": hue-
        // outermost, then sat, then val. So a stride of `sat * val * 3` per
        // hue slice, `val * 3` per sat column, `3` per val step.
        let idx = ((h_idx * sat + s_idx) * val + v_idx) as usize * 3;
        (self.data[idx], self.data[idx + 1], self.data[idx + 2])
    }
}

/// Parse errors. Each variant points to a concrete corruption mode with
/// enough detail to diagnose without a hex dump.
#[derive(Debug)]
pub enum ParseError {
    /// File too small to contain even the 8-byte header.
    TooShort,
    /// First 4 bytes aren't the ASCII `IIRC` magic.
    BadMagic,
    /// `ifd_offset` points outside the buffer.
    BadIfdOffset,
    /// An IFD entry points to a data blob outside the buffer, or the claimed
    /// element count overflows.
    BadEntryOffset { tag: u16 },
    /// A hue/sat map's dimension tag is malformed or the data length doesn't
    /// match `hue × sat × val × 3 × sizeof(f32)`.
    BadHueSatMap,
    /// An IFD claims more entries than the buffer can possibly hold. Guards
    /// against adversarial inputs.
    ImplausibleEntryCount,
    /// `ProfileToneCurve` (50940) has a byte length that overflows on multiply.
    BadToneCurve,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => write!(f, "file too short to be a DCP"),
            Self::BadMagic => write!(f, "missing `IIRC` magic"),
            Self::BadIfdOffset => write!(f, "IFD offset points outside buffer"),
            Self::BadEntryOffset { tag } => {
                write!(f, "entry data out of bounds for tag {tag:#06x}")
            }
            Self::BadHueSatMap => write!(f, "hue/sat map dimensions or payload malformed"),
            Self::ImplausibleEntryCount => write!(f, "implausible IFD entry count"),
            Self::BadToneCurve => write!(f, "tone curve payload overflows"),
        }
    }
}

impl std::error::Error for ParseError {}

// ----- tag numeric constants -----
// We hard-code these rather than leaning on `rawler::tags::DngTag` so the
// parser stays cheap to read against the DNG spec. Every number here maps
// one-to-one to a tag documented in Adobe DNG 1.6 § 6.2.

const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_CALIBRATION_ILLUMINANT_1: u16 = 50778;
const TAG_CALIBRATION_ILLUMINANT_2: u16 = 50779;
const TAG_PROFILE_CALIBRATION_SIGNATURE: u16 = 50932;
const TAG_PROFILE_NAME: u16 = 50936;
const TAG_PROFILE_HUE_SAT_MAP_DIMS: u16 = 50937;
const TAG_PROFILE_HUE_SAT_MAP_DATA_1: u16 = 50938;
const TAG_PROFILE_HUE_SAT_MAP_DATA_2: u16 = 50939;
const TAG_PROFILE_TONE_CURVE: u16 = 50940;
const TAG_PROFILE_COPYRIGHT: u16 = 50942;
const TAG_PROFILE_LOOK_TABLE_DIMS: u16 = 50981;
const TAG_PROFILE_LOOK_TABLE_DATA: u16 = 50982;
const TAG_PROFILE_HUE_SAT_MAP_ENCODING: u16 = 51107;
const TAG_PROFILE_LOOK_TABLE_ENCODING: u16 = 51108;

// TIFF type codes used by the tags above.
const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_FLOAT: u16 = 11;

/// Parse a `.dcp` file into a [`Dcp`] struct. Returns `Err` for anything the
/// caller should react to; unknown tags are silently skipped.
pub fn parse(bytes: &[u8]) -> Result<Dcp, ParseError> {
    if bytes.len() < 8 {
        return Err(ParseError::TooShort);
    }
    if &bytes[0..4] != b"IIRC" {
        return Err(ParseError::BadMagic);
    }
    let ifd_offset = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if ifd_offset + 2 > bytes.len() {
        return Err(ParseError::BadIfdOffset);
    }

    let entry_count = u16::from_le_bytes([bytes[ifd_offset], bytes[ifd_offset + 1]]) as usize;
    // Every entry is 12 bytes; refuse counts that can't possibly fit.
    if ifd_offset + 2 + entry_count * 12 > bytes.len() {
        return Err(ParseError::ImplausibleEntryCount);
    }

    let mut dcp = Dcp {
        unique_camera_model: None,
        profile_name: None,
        profile_copyright: None,
        profile_calibration_signature: None,
        calibration_illuminant_1: None,
        calibration_illuminant_2: None,
        hue_sat_map_1: None,
        hue_sat_map_2: None,
        hue_sat_map_encoding: 0,
        look_table: None,
        look_table_encoding: 0,
        tone_curve: None,
    };

    // First pass: collect entries. We need the dims tag resolved before we
    // can interpret the data tags, so collect everything into a side table
    // and then unpack at the end.
    let mut hsm_dims: Option<[u32; 3]> = None;
    let mut hsm_data_1_offset: Option<(usize, usize)> = None;
    let mut hsm_data_2_offset: Option<(usize, usize)> = None;
    let mut look_dims: Option<[u32; 3]> = None;
    let mut look_data_offset: Option<(usize, usize)> = None;

    for i in 0..entry_count {
        let eo = ifd_offset + 2 + i * 12;
        let tag = u16::from_le_bytes([bytes[eo], bytes[eo + 1]]);
        let typ = u16::from_le_bytes([bytes[eo + 2], bytes[eo + 3]]);
        let count = u32::from_le_bytes([bytes[eo + 4], bytes[eo + 5], bytes[eo + 6], bytes[eo + 7]])
            as usize;
        let value_field = &bytes[eo + 8..eo + 12];

        match tag {
            TAG_UNIQUE_CAMERA_MODEL if typ == TYPE_ASCII => {
                dcp.unique_camera_model = read_ascii(bytes, value_field, count)?
                    .map(|s| s.trim_end_matches('\0').to_string());
            }
            TAG_PROFILE_NAME if typ == TYPE_ASCII => {
                dcp.profile_name = read_ascii(bytes, value_field, count)?
                    .map(|s| s.trim_end_matches('\0').to_string());
            }
            TAG_PROFILE_COPYRIGHT if typ == TYPE_ASCII => {
                dcp.profile_copyright = read_ascii(bytes, value_field, count)?
                    .map(|s| s.trim_end_matches('\0').to_string());
            }
            TAG_PROFILE_CALIBRATION_SIGNATURE if typ == TYPE_ASCII => {
                dcp.profile_calibration_signature = read_ascii(bytes, value_field, count)?
                    .map(|s| s.trim_end_matches('\0').to_string());
            }
            TAG_CALIBRATION_ILLUMINANT_1 if typ == TYPE_SHORT && count == 1 => {
                dcp.calibration_illuminant_1 =
                    Some(u16::from_le_bytes([value_field[0], value_field[1]]));
            }
            TAG_CALIBRATION_ILLUMINANT_2 if typ == TYPE_SHORT && count == 1 => {
                dcp.calibration_illuminant_2 =
                    Some(u16::from_le_bytes([value_field[0], value_field[1]]));
            }
            TAG_PROFILE_HUE_SAT_MAP_DIMS if typ == TYPE_LONG && count == 3 => {
                // 3 × u32 LONGs = 12 bytes, doesn't fit in the 4-byte inline
                // value field, so it's always out-of-line.
                let off = u32::from_le_bytes([
                    value_field[0],
                    value_field[1],
                    value_field[2],
                    value_field[3],
                ]) as usize;
                if off + 12 > bytes.len() {
                    return Err(ParseError::BadEntryOffset { tag });
                }
                let h = u32::from_le_bytes([
                    bytes[off],
                    bytes[off + 1],
                    bytes[off + 2],
                    bytes[off + 3],
                ]);
                let s = u32::from_le_bytes([
                    bytes[off + 4],
                    bytes[off + 5],
                    bytes[off + 6],
                    bytes[off + 7],
                ]);
                let v = u32::from_le_bytes([
                    bytes[off + 8],
                    bytes[off + 9],
                    bytes[off + 10],
                    bytes[off + 11],
                ]);
                hsm_dims = Some([h, s, v]);
            }
            TAG_PROFILE_HUE_SAT_MAP_DATA_1 if typ == TYPE_FLOAT => {
                // The offset field points into the buffer. 4-byte floats
                // with count ≥ 2 are always out-of-line (min 8 bytes > 4).
                let off = u32::from_le_bytes([
                    value_field[0],
                    value_field[1],
                    value_field[2],
                    value_field[3],
                ]) as usize;
                hsm_data_1_offset = Some((off, count));
            }
            TAG_PROFILE_HUE_SAT_MAP_DATA_2 if typ == TYPE_FLOAT => {
                let off = u32::from_le_bytes([
                    value_field[0],
                    value_field[1],
                    value_field[2],
                    value_field[3],
                ]) as usize;
                hsm_data_2_offset = Some((off, count));
            }
            TAG_PROFILE_HUE_SAT_MAP_ENCODING if typ == TYPE_LONG && count == 1 => {
                dcp.hue_sat_map_encoding = u32::from_le_bytes([
                    value_field[0],
                    value_field[1],
                    value_field[2],
                    value_field[3],
                ]);
            }
            TAG_PROFILE_LOOK_TABLE_DIMS if typ == TYPE_LONG && count == 3 => {
                // Same 3×u32 layout as HueSatMap dims; always out-of-line.
                let off = u32::from_le_bytes([
                    value_field[0],
                    value_field[1],
                    value_field[2],
                    value_field[3],
                ]) as usize;
                if off + 12 > bytes.len() {
                    return Err(ParseError::BadEntryOffset { tag });
                }
                let h = u32::from_le_bytes([
                    bytes[off],
                    bytes[off + 1],
                    bytes[off + 2],
                    bytes[off + 3],
                ]);
                let s = u32::from_le_bytes([
                    bytes[off + 4],
                    bytes[off + 5],
                    bytes[off + 6],
                    bytes[off + 7],
                ]);
                let v = u32::from_le_bytes([
                    bytes[off + 8],
                    bytes[off + 9],
                    bytes[off + 10],
                    bytes[off + 11],
                ]);
                look_dims = Some([h, s, v]);
            }
            TAG_PROFILE_LOOK_TABLE_DATA if typ == TYPE_FLOAT => {
                let off = u32::from_le_bytes([
                    value_field[0],
                    value_field[1],
                    value_field[2],
                    value_field[3],
                ]) as usize;
                look_data_offset = Some((off, count));
            }
            TAG_PROFILE_LOOK_TABLE_ENCODING if typ == TYPE_LONG && count == 1 => {
                dcp.look_table_encoding = u32::from_le_bytes([
                    value_field[0],
                    value_field[1],
                    value_field[2],
                    value_field[3],
                ]);
            }
            TAG_PROFILE_TONE_CURVE if typ == TYPE_FLOAT => {
                // A list of (x, y) pairs, so count is 2 × number_of_points.
                // Always out-of-line (min count is 4 for the required
                // endpoints, 4 × 4 bytes = 16 > 4-byte inline slot).
                let off = u32::from_le_bytes([
                    value_field[0],
                    value_field[1],
                    value_field[2],
                    value_field[3],
                ]) as usize;
                if count < 4 || !count.is_multiple_of(2) {
                    // Spec requires at least the two endpoints, and points
                    // come in pairs. Malformed → skip, don't abort the
                    // whole parse.
                    continue;
                }
                let byte_len = count.checked_mul(4).ok_or(ParseError::BadToneCurve)?;
                if off.checked_add(byte_len).is_none_or(|e| e > bytes.len()) {
                    return Err(ParseError::BadEntryOffset { tag });
                }
                let mut points = Vec::with_capacity(count / 2);
                for i in 0..(count / 2) {
                    let xo = off + i * 8;
                    let yo = xo + 4;
                    let x = f32::from_le_bytes([
                        bytes[xo],
                        bytes[xo + 1],
                        bytes[xo + 2],
                        bytes[xo + 3],
                    ]);
                    let y = f32::from_le_bytes([
                        bytes[yo],
                        bytes[yo + 1],
                        bytes[yo + 2],
                        bytes[yo + 3],
                    ]);
                    points.push((x, y));
                }
                dcp.tone_curve = Some(points);
            }
            _ => {
                // Silently ignore unknown tags. Lots of DCPs in the wild
                // carry vendor-specific extras and we don't want to log-spam
                // every time.
            }
        }
    }

    // Second pass: reconstruct hue/sat maps from dims + data blocks.
    if let Some([h, s, v]) = hsm_dims {
        let expected_count = (h as usize)
            .checked_mul(s as usize)
            .and_then(|x| x.checked_mul(v as usize))
            .and_then(|x| x.checked_mul(3))
            .ok_or(ParseError::BadHueSatMap)?;
        if let Some((off, count)) = hsm_data_1_offset {
            dcp.hue_sat_map_1 = Some(read_hue_sat_map(
                bytes,
                off,
                count,
                expected_count,
                h,
                s,
                v,
            )?);
        }
        if let Some((off, count)) = hsm_data_2_offset {
            dcp.hue_sat_map_2 = Some(read_hue_sat_map(
                bytes,
                off,
                count,
                expected_count,
                h,
                s,
                v,
            )?);
        }
    }

    // Look table: same shape as HueSatMap. Single-illuminant (there is no
    // LookTableData2 — the profile ships one look for all illuminants).
    if let (Some([h, s, v]), Some((off, count))) = (look_dims, look_data_offset) {
        let expected_count = (h as usize)
            .checked_mul(s as usize)
            .and_then(|x| x.checked_mul(v as usize))
            .and_then(|x| x.checked_mul(3))
            .ok_or(ParseError::BadHueSatMap)?;
        dcp.look_table = Some(read_hue_sat_map(
            bytes,
            off,
            count,
            expected_count,
            h,
            s,
            v,
        )?);
    }

    Ok(dcp)
}

fn read_hue_sat_map(
    bytes: &[u8],
    offset: usize,
    count: usize,
    expected_count: usize,
    hue_divs: u32,
    sat_divs: u32,
    val_divs: u32,
) -> Result<HueSatMap, ParseError> {
    if count != expected_count {
        return Err(ParseError::BadHueSatMap);
    }
    let byte_len = count.checked_mul(4).ok_or(ParseError::BadHueSatMap)?;
    if offset
        .checked_add(byte_len)
        .is_none_or(|end| end > bytes.len())
    {
        return Err(ParseError::BadHueSatMap);
    }
    let mut data = Vec::with_capacity(count);
    for i in 0..count {
        let bo = offset + i * 4;
        data.push(f32::from_le_bytes([
            bytes[bo],
            bytes[bo + 1],
            bytes[bo + 2],
            bytes[bo + 3],
        ]));
    }
    Ok(HueSatMap {
        hue_divs,
        sat_divs,
        val_divs,
        data,
    })
}

fn read_ascii(
    bytes: &[u8],
    value_field: &[u8],
    count: usize,
) -> Result<Option<String>, ParseError> {
    if count == 0 {
        return Ok(None);
    }
    // ASCII with count ≤ 4 fits inline in the 4-byte value field.
    let slice = if count <= 4 {
        &value_field[..count]
    } else {
        let off = u32::from_le_bytes([
            value_field[0],
            value_field[1],
            value_field[2],
            value_field[3],
        ]) as usize;
        if off + count > bytes.len() {
            return Err(ParseError::BadEntryOffset { tag: 0 });
        }
        &bytes[off..off + count]
    };
    // DNG ASCII is null-terminated; strip the terminator before utf-8
    // decoding so we don't pollute the string with a trailing NUL.
    let trimmed = slice.split(|b| *b == 0).next().unwrap_or(slice);
    Ok(Some(String::from_utf8_lossy(trimmed).trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal DCP with a single hue/sat map. Handy for unit tests.
    /// Returns `(bytes, unique_camera_model)`.
    ///
    /// Layout:
    /// - header (8 B)
    /// - IFD count (2 B) + entries
    /// - out-of-line payload (ASCII strings, dims, f32 data)
    fn build_minimal_dcp(
        unique_model: &str,
        hue_divs: u32,
        sat_divs: u32,
        val_divs: u32,
        data: &[f32],
        illuminant: u16,
    ) -> Vec<u8> {
        assert_eq!(
            data.len(),
            (hue_divs * sat_divs * val_divs * 3) as usize,
            "data length must match dims"
        );

        // Compute offsets up front. Start of IFD = 8. IFD has 5 entries
        // (UniqueCameraModel, CalibrationIlluminant1, ProfileHueSatMapDims,
        // ProfileHueSatMapData1, ProfileName), plus 4-byte next-IFD
        // terminator. Entries = 5 × 12 = 60 bytes, plus 2 count, plus 4
        // next-IFD = 66. First out-of-line payload sits at offset 8 + 66 = 74.
        let num_entries: u16 = 5;
        let ifd_end = 8 + 2 + (num_entries as usize) * 12 + 4;

        let ucm_bytes = {
            let mut v = unique_model.as_bytes().to_vec();
            v.push(0);
            v
        };
        let ucm_offset = ifd_end;
        let ucm_count = ucm_bytes.len();

        let profile_name = "Test Profile";
        let pn_bytes = {
            let mut v = profile_name.as_bytes().to_vec();
            v.push(0);
            v
        };
        let pn_offset = ucm_offset + ucm_count;
        let pn_count = pn_bytes.len();

        let dims_offset = pn_offset + pn_count;
        let dims_bytes: Vec<u8> = [hue_divs, sat_divs, val_divs]
            .iter()
            .flat_map(|v| v.to_le_bytes().to_vec())
            .collect();

        let data_offset = dims_offset + 12;
        let data_bytes: Vec<u8> = data.iter().flat_map(|f| f.to_le_bytes().to_vec()).collect();

        let total_len = data_offset + data_bytes.len();
        let mut out = vec![0u8; total_len];
        out[0..4].copy_from_slice(b"IIRC");
        out[4..8].copy_from_slice(&8u32.to_le_bytes());

        // IFD count
        out[8..10].copy_from_slice(&num_entries.to_le_bytes());

        // Entry 0: UniqueCameraModel, ASCII
        let mut eo = 10;
        write_entry(
            &mut out,
            eo,
            TAG_UNIQUE_CAMERA_MODEL,
            TYPE_ASCII,
            ucm_count as u32,
            ucm_offset as u32,
        );
        eo += 12;
        // Entry 1: ProfileName, ASCII
        write_entry(
            &mut out,
            eo,
            TAG_PROFILE_NAME,
            TYPE_ASCII,
            pn_count as u32,
            pn_offset as u32,
        );
        eo += 12;
        // Entry 2: CalibrationIlluminant1, SHORT (inline)
        let mut inline = [0u8; 4];
        inline[0..2].copy_from_slice(&illuminant.to_le_bytes());
        write_entry_inline(
            &mut out,
            eo,
            TAG_CALIBRATION_ILLUMINANT_1,
            TYPE_SHORT,
            1,
            inline,
        );
        eo += 12;
        // Entry 3: ProfileHueSatMapDims, LONG (3 values, out-of-line)
        write_entry(
            &mut out,
            eo,
            TAG_PROFILE_HUE_SAT_MAP_DIMS,
            TYPE_LONG,
            3,
            dims_offset as u32,
        );
        eo += 12;
        // Entry 4: ProfileHueSatMapData1, FLOAT
        write_entry(
            &mut out,
            eo,
            TAG_PROFILE_HUE_SAT_MAP_DATA_1,
            TYPE_FLOAT,
            data.len() as u32,
            data_offset as u32,
        );

        // Payload
        out[ucm_offset..ucm_offset + ucm_count].copy_from_slice(&ucm_bytes);
        out[pn_offset..pn_offset + pn_count].copy_from_slice(&pn_bytes);
        out[dims_offset..dims_offset + 12].copy_from_slice(&dims_bytes);
        out[data_offset..data_offset + data_bytes.len()].copy_from_slice(&data_bytes);

        out
    }

    fn write_entry(
        out: &mut [u8],
        eo: usize,
        tag: u16,
        typ: u16,
        count: u32,
        value_or_offset: u32,
    ) {
        out[eo..eo + 2].copy_from_slice(&tag.to_le_bytes());
        out[eo + 2..eo + 4].copy_from_slice(&typ.to_le_bytes());
        out[eo + 4..eo + 8].copy_from_slice(&count.to_le_bytes());
        out[eo + 8..eo + 12].copy_from_slice(&value_or_offset.to_le_bytes());
    }

    fn write_entry_inline(
        out: &mut [u8],
        eo: usize,
        tag: u16,
        typ: u16,
        count: u32,
        inline: [u8; 4],
    ) {
        out[eo..eo + 2].copy_from_slice(&tag.to_le_bytes());
        out[eo + 2..eo + 4].copy_from_slice(&typ.to_le_bytes());
        out[eo + 4..eo + 8].copy_from_slice(&count.to_le_bytes());
        out[eo + 8..eo + 12].copy_from_slice(&inline);
    }

    #[test]
    fn rejects_too_short() {
        assert!(matches!(parse(&[]), Err(ParseError::TooShort)));
        assert!(matches!(parse(&[0u8; 5]), Err(ParseError::TooShort)));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(b"II\x2a\x00"); // plain TIFF magic, not DCP
        assert!(matches!(parse(&bytes), Err(ParseError::BadMagic)));
    }

    #[test]
    fn rejects_bad_ifd_offset() {
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(b"IIRC");
        bytes[4..8].copy_from_slice(&1_000_000u32.to_le_bytes());
        assert!(matches!(parse(&bytes), Err(ParseError::BadIfdOffset)));
    }

    #[test]
    fn parses_minimal_dcp() {
        // 2×2×1 identity-ish HSM: hue_shift=0, sat_scale=1, val_scale=1 for
        // every entry.
        let data = vec![0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0];
        let bytes = build_minimal_dcp("Test Camera", 2, 2, 1, &data, 21);
        let dcp = parse(&bytes).expect("parse should succeed");
        assert_eq!(dcp.unique_camera_model.as_deref(), Some("Test Camera"));
        assert_eq!(dcp.profile_name.as_deref(), Some("Test Profile"));
        assert_eq!(dcp.calibration_illuminant_1, Some(21));
        let map = dcp.hue_sat_map_1.expect("HueSatMap1 should be present");
        assert_eq!(map.hue_divs, 2);
        assert_eq!(map.sat_divs, 2);
        assert_eq!(map.val_divs, 1);
        assert_eq!(map.data, data);
    }

    #[test]
    fn pick_hue_sat_map_prefers_d65() {
        // Map 1 on Standard Light A (17), map 2 on D65 (21) -> pick map 2.
        let dcp = Dcp {
            unique_camera_model: None,
            profile_name: None,
            profile_copyright: None,
            profile_calibration_signature: None,
            calibration_illuminant_1: Some(17),
            calibration_illuminant_2: Some(21),
            hue_sat_map_1: Some(HueSatMap {
                hue_divs: 1,
                sat_divs: 1,
                val_divs: 1,
                data: vec![10.0, 2.0, 2.0], // distinctive
            }),
            hue_sat_map_2: Some(HueSatMap {
                hue_divs: 1,
                sat_divs: 1,
                val_divs: 1,
                data: vec![20.0, 3.0, 3.0], // distinctive
            }),
            hue_sat_map_encoding: 0,
            look_table: None,
            look_table_encoding: 0,
            tone_curve: None,
        };
        let picked = dcp.pick_hue_sat_map().expect("should pick one");
        assert_eq!(picked.data, vec![20.0, 3.0, 3.0]);
    }

    #[test]
    fn pick_hue_sat_map_falls_back_to_single() {
        let dcp = Dcp {
            unique_camera_model: None,
            profile_name: None,
            profile_copyright: None,
            profile_calibration_signature: None,
            calibration_illuminant_1: Some(17),
            calibration_illuminant_2: None,
            hue_sat_map_1: Some(HueSatMap {
                hue_divs: 1,
                sat_divs: 1,
                val_divs: 1,
                data: vec![5.0, 1.5, 1.5],
            }),
            hue_sat_map_2: None,
            hue_sat_map_encoding: 0,
            look_table: None,
            look_table_encoding: 0,
            tone_curve: None,
        };
        let picked = dcp.pick_hue_sat_map().expect("should pick map 1");
        assert_eq!(picked.data, vec![5.0, 1.5, 1.5]);
    }

    /// Build a DCP that includes optional `ProfileLookTable*` and
    /// `ProfileToneCurve` alongside the required HueSatMap. Shares its IFD
    /// scaffolding with [`build_minimal_dcp`] but adds three more entries.
    fn build_dcp_with_look_and_curve(
        hsm_dims: [u32; 3],
        hsm_data: &[f32],
        look_dims: [u32; 3],
        look_data: &[f32],
        curve_points: &[(f32, f32)],
    ) -> Vec<u8> {
        // Seven entries: UniqueCameraModel, ProfileName, HSM dims, HSM data
        // 1, LookTable dims, LookTable data, ToneCurve.
        let num_entries: u16 = 7;
        let ifd_end = 8 + 2 + (num_entries as usize) * 12 + 4;

        let ucm_bytes = b"Test Camera\0".to_vec();
        let ucm_offset = ifd_end;
        let ucm_count = ucm_bytes.len();

        let pn_bytes = b"Test Profile\0".to_vec();
        let pn_offset = ucm_offset + ucm_count;
        let pn_count = pn_bytes.len();

        let dims_offset = pn_offset + pn_count;
        let dims_bytes: Vec<u8> = hsm_dims
            .iter()
            .flat_map(|v| v.to_le_bytes().to_vec())
            .collect();

        let hsm_data_offset = dims_offset + 12;
        let hsm_data_bytes: Vec<u8> = hsm_data
            .iter()
            .flat_map(|f| f.to_le_bytes().to_vec())
            .collect();

        let look_dims_offset = hsm_data_offset + hsm_data_bytes.len();
        let look_dims_bytes: Vec<u8> = look_dims
            .iter()
            .flat_map(|v| v.to_le_bytes().to_vec())
            .collect();

        let look_data_offset = look_dims_offset + 12;
        let look_data_bytes: Vec<u8> = look_data
            .iter()
            .flat_map(|f| f.to_le_bytes().to_vec())
            .collect();

        let curve_offset = look_data_offset + look_data_bytes.len();
        let curve_floats: Vec<f32> = curve_points.iter().flat_map(|(x, y)| [*x, *y]).collect();
        let curve_bytes: Vec<u8> = curve_floats
            .iter()
            .flat_map(|f| f.to_le_bytes().to_vec())
            .collect();

        let total_len = curve_offset + curve_bytes.len();
        let mut out = vec![0u8; total_len];
        out[0..4].copy_from_slice(b"IIRC");
        out[4..8].copy_from_slice(&8u32.to_le_bytes());
        out[8..10].copy_from_slice(&num_entries.to_le_bytes());

        let mut eo = 10;
        write_entry(
            &mut out,
            eo,
            TAG_UNIQUE_CAMERA_MODEL,
            TYPE_ASCII,
            ucm_count as u32,
            ucm_offset as u32,
        );
        eo += 12;
        write_entry(
            &mut out,
            eo,
            TAG_PROFILE_NAME,
            TYPE_ASCII,
            pn_count as u32,
            pn_offset as u32,
        );
        eo += 12;
        write_entry(
            &mut out,
            eo,
            TAG_PROFILE_HUE_SAT_MAP_DIMS,
            TYPE_LONG,
            3,
            dims_offset as u32,
        );
        eo += 12;
        write_entry(
            &mut out,
            eo,
            TAG_PROFILE_HUE_SAT_MAP_DATA_1,
            TYPE_FLOAT,
            hsm_data.len() as u32,
            hsm_data_offset as u32,
        );
        eo += 12;
        write_entry(
            &mut out,
            eo,
            TAG_PROFILE_LOOK_TABLE_DIMS,
            TYPE_LONG,
            3,
            look_dims_offset as u32,
        );
        eo += 12;
        write_entry(
            &mut out,
            eo,
            TAG_PROFILE_LOOK_TABLE_DATA,
            TYPE_FLOAT,
            look_data.len() as u32,
            look_data_offset as u32,
        );
        eo += 12;
        write_entry(
            &mut out,
            eo,
            TAG_PROFILE_TONE_CURVE,
            TYPE_FLOAT,
            curve_floats.len() as u32,
            curve_offset as u32,
        );

        out[ucm_offset..ucm_offset + ucm_count].copy_from_slice(&ucm_bytes);
        out[pn_offset..pn_offset + pn_count].copy_from_slice(&pn_bytes);
        out[dims_offset..dims_offset + 12].copy_from_slice(&dims_bytes);
        out[hsm_data_offset..hsm_data_offset + hsm_data_bytes.len()]
            .copy_from_slice(&hsm_data_bytes);
        out[look_dims_offset..look_dims_offset + 12].copy_from_slice(&look_dims_bytes);
        out[look_data_offset..look_data_offset + look_data_bytes.len()]
            .copy_from_slice(&look_data_bytes);
        out[curve_offset..curve_offset + curve_bytes.len()].copy_from_slice(&curve_bytes);

        out
    }

    #[test]
    fn parses_dcp_with_look_table_and_tone_curve() {
        let hsm_data = vec![
            0.0_f32, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0,
        ];
        let look_data = vec![
            5.0_f32, 1.2, 1.0, 5.0, 1.2, 1.0, 5.0, 1.2, 1.0, 5.0, 1.2, 1.0,
        ];
        let curve = vec![(0.0, 0.0), (0.5, 0.45), (1.0, 1.0)];
        let bytes =
            build_dcp_with_look_and_curve([2, 2, 1], &hsm_data, [2, 2, 1], &look_data, &curve);
        let dcp = parse(&bytes).expect("parse should succeed");
        // HueSatMap still there.
        let hsm = dcp.hue_sat_map_1.expect("hsm present");
        assert_eq!((hsm.hue_divs, hsm.sat_divs, hsm.val_divs), (2, 2, 1));
        // LookTable carried through.
        let look = dcp.look_table.expect("look present");
        assert_eq!((look.hue_divs, look.sat_divs, look.val_divs), (2, 2, 1));
        assert_eq!(look.data, look_data);
        // ToneCurve carried through.
        let tc = dcp.tone_curve.expect("tone curve present");
        assert_eq!(tc, curve);
    }

    #[test]
    fn rejects_implausible_entry_count() {
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(b"IIRC");
        bytes[4..8].copy_from_slice(&8u32.to_le_bytes());
        // Claim 30_000 entries in an 8-byte remaining buffer.
        bytes[8..10].copy_from_slice(&30_000u16.to_le_bytes());
        assert!(matches!(
            parse(&bytes),
            Err(ParseError::ImplausibleEntryCount)
        ));
    }

    /// Parses a local DCP from `/tmp/prvw-dcp-test/`, if present, to confirm
    /// the parser handles a real-world Adobe-format file. Ignored by default
    /// so CI (which has no DCP fixture) stays green; run manually with
    /// `cargo test -- --ignored real_world_dcp_parses`.
    #[test]
    #[ignore]
    fn real_world_dcp_parses() {
        let path = "/tmp/prvw-dcp-test/SONY_ILCE-7M3.dcp";
        if !std::path::Path::new(path).exists() {
            eprintln!(
                "Skipping {path}: file missing. Download from RawTherapee's rtdata/dcpprofiles to run."
            );
            return;
        }
        let bytes = std::fs::read(path).expect("read dcp");
        let dcp = parse(&bytes).expect("parse real-world DCP");
        assert_eq!(dcp.unique_camera_model.as_deref(), Some("Sony ILCE-7M3"));
        assert_eq!(dcp.profile_name.as_deref(), Some("SONY ILCE-7M3"));
        assert_eq!(dcp.calibration_illuminant_1, Some(17));
        assert_eq!(dcp.calibration_illuminant_2, Some(21));
        let m1 = dcp.hue_sat_map_1.as_ref().expect("hsm1");
        assert_eq!((m1.hue_divs, m1.sat_divs, m1.val_divs), (90, 30, 1));
        assert_eq!(m1.data.len(), (90 * 30) * 3);
        // First entry is always the "neutral" row: (0°, 1.0, 1.0).
        assert_eq!(m1.data[0], 0.0);
        assert_eq!(m1.data[1], 1.0);
        assert_eq!(m1.data[2], 1.0);
        // Pick-map should prefer the D65 slot (illuminant 2 = 21).
        let picked = dcp.pick_hue_sat_map().expect("picked");
        assert_eq!(
            picked.data.len(),
            dcp.hue_sat_map_2.as_ref().unwrap().data.len()
        );
    }

    /// Bench: parse a real DCP. Ignored, run with
    /// `cargo test --release parse_bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn parse_bench() {
        let path = "/tmp/prvw-dcp-test/SONY_ILCE-7M3.dcp";
        if !std::path::Path::new(path).exists() {
            eprintln!("Skipping: {path} missing");
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        let mut times = vec![];
        for _ in 0..10 {
            let t = std::time::Instant::now();
            let _ = parse(&bytes).expect("parse");
            times.push(t.elapsed().as_micros());
        }
        println!("DCP parse (µs): {times:?}");
    }

    #[test]
    fn huesat_map_sample_at_corners() {
        // 2×2×1 map where each corner is distinct.
        let data = vec![
            1.0, 1.1, 1.2, // (h=0, s=0, v=0)
            2.0, 2.1, 2.2, // (h=0, s=1, v=0)
            3.0, 3.1, 3.2, // (h=1, s=0, v=0)
            4.0, 4.1, 4.2, // (h=1, s=1, v=0)
        ];
        let m = HueSatMap {
            hue_divs: 2,
            sat_divs: 2,
            val_divs: 1,
            data,
        };
        assert_eq!(m.sample(0, 0, 0), (1.0, 1.1, 1.2));
        assert_eq!(m.sample(0, 1, 0), (2.0, 2.1, 2.2));
        assert_eq!(m.sample(1, 0, 0), (3.0, 3.1, 3.2));
        assert_eq!(m.sample(1, 1, 0), (4.0, 4.1, 4.2));
    }
}
