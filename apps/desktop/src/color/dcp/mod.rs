//! Adobe DCP (Digital Camera Profile) support.
//!
//! A DCP captures per-camera color refinement that a generic 3×3 matrix
//! can't. It's Adobe's recipe for "my Sony A7 III renders skin tones like
//! this, saturated reds roll off like that" and is the biggest single
//! quality lift Lightroom gets over a naïve matrix-only develop. This
//! module parses `.dcp` files, discovers one matching the current camera,
//! and applies the `ProfileHueSatMap` 3D LUT to our linear Rec.2020 buffer.
//!
//! ## What's implemented
//!
//! - Parser for the DCP TIFF-like format (`IIRC` magic).
//! - Extraction of `ProfileName`, `ProfileCopyright`,
//!   `UniqueCameraModel`, `ProfileCalibrationSignature`,
//!   `CalibrationIlluminant1/2`, `ProfileHueSatMapDims`,
//!   `ProfileHueSatMapData1/2`, and `ProfileHueSatMapEncoding`.
//! - Trilinear HSV LUT application with cyclic hue axis and clamped
//!   sat / val axes.
//! - Discovery from `$PRVW_DCP_DIR` and
//!   `~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/`,
//!   with graceful fallback to the default pipeline when no match is
//!   found (and no ACR install is required).
//!
//! ## What's deferred
//!
//! - **LookTable** (`ProfileLookTableData`). Same shape and application
//!   math as HueSatMap but applied as a second pass; low priority because
//!   HueSatMap alone captures the bulk of the per-camera color
//!   refinement.
//! - **ProfileToneCurve**. Our default luminance-only tone curve is
//!   already doing the heavy lifting; swapping it in per image would
//!   change the baseline contrast of the whole app.
//! - **Forward matrix swap** (`ForwardMatrix1/2`). Our Rec.2020 path
//!   targets Rec.2020; DCP forward matrices target ProPhoto D50. A
//!   proper swap would need a full chromatic-adaptation re-pipe.
//! - **Dual-illuminant interpolation**. We currently pick the D65 map
//!   (illuminant 21) straight through. Adobe's color-temperature-aware
//!   interpolation between illuminants 1 and 2 is the main phase-3.x
//!   upgrade path.
//!
//! ## Pipeline slot
//!
//! The DCP HueSatMap runs in linear Rec.2020, **after** highlight
//! recovery and **before** the default tone curve. Same slot as
//! Lightroom's "Camera Calibration" pane: before the response shaping,
//! after the WB+matrix assembly. See `crate::decoding::raw`.

pub mod apply;
pub mod discovery;
pub mod parser;

pub use apply::apply_hue_sat_map;
pub use discovery::{find_dcp_for_camera, log_search_summary_once};
pub use parser::Dcp;

/// End-to-end "load a DCP and apply it" helper. Pass the camera's
/// `"<Make> <Model>"` identity string and the linear-Rec.2020 buffer;
/// returns `true` if a DCP was found and applied, `false` if we no-op'd.
///
/// The `true` return is how the decoder logs "DCP was applied" without
/// duplicating the discovery logic.
pub fn apply_if_available(camera_id: &str, rgb: &mut [f32]) -> Option<Dcp> {
    log_search_summary_once();
    let dcp = find_dcp_for_camera(camera_id)?;
    let map = dcp.pick_hue_sat_map()?;
    apply_hue_sat_map(rgb, map, dcp.hue_sat_map_encoding);
    Some(dcp)
}

#[cfg(test)]
pub(crate) mod tests {
    //! Small test helpers shared by the submodules. Kept here so test-only
    //! code doesn't pollute the public API.

    /// Build a tiny identity-ish DCP with the supplied `UniqueCameraModel`.
    /// The HueSatMap has hue shifts = 0 and sat / val scales = 1, so
    /// applying it is a no-op.
    pub fn tiny_identity_dcp(unique_camera_model: &str) -> Vec<u8> {
        // 4 entries (h=2, s=2, v=1) keeps the file tiny but still fully
        // trilinearly interpolable. The parser's build-time test helper
        // needs private access, so we rebuild the same layout here using
        // public constants via `super::parser`. Slight duplication, but
        // keeps this helper easy to reach from multiple test modules.
        let data = [
            0.0_f32, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0,
        ];
        build_dcp_with_map(unique_camera_model, 2, 2, 1, &data, 21)
    }

    fn build_dcp_with_map(
        unique_model: &str,
        hue_divs: u32,
        sat_divs: u32,
        val_divs: u32,
        data: &[f32],
        illuminant: u16,
    ) -> Vec<u8> {
        // See `parser::tests::build_minimal_dcp` for the exhaustively
        // commented version; this is the same layout in a shared helper.
        const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
        const TAG_CALIBRATION_ILLUMINANT_1: u16 = 50778;
        const TAG_PROFILE_NAME: u16 = 50936;
        const TAG_PROFILE_HUE_SAT_MAP_DIMS: u16 = 50937;
        const TAG_PROFILE_HUE_SAT_MAP_DATA_1: u16 = 50938;
        const TYPE_ASCII: u16 = 2;
        const TYPE_SHORT: u16 = 3;
        const TYPE_LONG: u16 = 4;
        const TYPE_FLOAT: u16 = 11;

        let num_entries: u16 = 5;
        let ifd_end = 8 + 2 + (num_entries as usize) * 12 + 4;

        let ucm_bytes = {
            let mut v = unique_model.as_bytes().to_vec();
            v.push(0);
            v
        };
        let ucm_offset = ifd_end;
        let ucm_count = ucm_bytes.len();

        let pn_bytes = b"Test\0".to_vec();
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
            data.len() as u32,
            data_offset as u32,
        );

        out[ucm_offset..ucm_offset + ucm_count].copy_from_slice(&ucm_bytes);
        out[pn_offset..pn_offset + pn_count].copy_from_slice(&pn_bytes);
        out[dims_offset..dims_offset + 12].copy_from_slice(&dims_bytes);
        out[data_offset..data_offset + data_bytes.len()].copy_from_slice(&data_bytes);

        out
    }

    fn write_entry(out: &mut [u8], eo: usize, tag: u16, typ: u16, count: u32, val: u32) {
        out[eo..eo + 2].copy_from_slice(&tag.to_le_bytes());
        out[eo + 2..eo + 4].copy_from_slice(&typ.to_le_bytes());
        out[eo + 4..eo + 8].copy_from_slice(&count.to_le_bytes());
        out[eo + 8..eo + 12].copy_from_slice(&val.to_le_bytes());
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
}
