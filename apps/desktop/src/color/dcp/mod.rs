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
//! - **Embedded profile reader** ([`from_dng_tags`]) for DNGs whose own
//!   main IFD carries the same profile tags. Smartphone DNGs (Pixel,
//!   Galaxy, iPhone ProRAW) and Adobe-converted DNGs all ship their
//!   profile this way.
//! - Extraction of `ProfileName`, `ProfileCopyright`,
//!   `UniqueCameraModel`, `ProfileCalibrationSignature`,
//!   `CalibrationIlluminant1/2`, `ProfileHueSatMapDims`,
//!   `ProfileHueSatMapData1/2`, and `ProfileHueSatMapEncoding`.
//! - Trilinear HSV LUT application with cyclic hue axis and clamped
//!   sat / val axes.
//! - Discovery from `$PRVW_DCP_DIR` and
//!   `~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/`,
//!   then from the bundled RawTherapee collection (Phase 3.5), with
//!   graceful fallback to the default pipeline when no match is found.
//! - **Bundled collection** (Phase 3.5): 161 RawTherapee community-
//!   contributed DCPs packed at build time into a gzip-compressed blob.
//!   They live in `color::dcp::bundled` and require zero user setup.
//! - **Fuzzy family matching** (Phase 3.5): when exact matching fails on
//!   all tiers, try known-compatible camera families from
//!   `color::dcp::family_aliases`. A conservative curated seed list.
//!   Logs at INFO so users see the substitution.
//!
//! ## Source precedence
//!
//! 1. **Embedded** — DNG's own IFD. Manufacturer profile; most trustworthy.
//! 2. **`$PRVW_DCP_DIR`** — user-provided override. Always beats bundled.
//! 3. **Adobe Camera Raw install dir** — system DCP library.
//! 4. **Bundled collection** — RawTherapee's 161 community profiles.
//! 5. **Fuzzy family alias** — tried across all tiers above for each alias.
//! 6. **None** — fall back to the default matrix-only pipeline.
//!
//! When a file carries both an embedded profile and a matching filesystem
//! DCP exists, **embedded wins**. The manufacturer baked that profile into
//! the file; it's the authoritative description of how the camera sees
//! color. Users who want to force the filesystem path can set
//! `PRVW_DISABLE_EMBEDDED_DCP=1`.
//!
//! ## What's covered since Phase 3.4
//!
//! - **LookTable** (`ProfileLookTableData`) applied post-HueSatMap,
//!   pre-tone-curve. Same algorithm, same apply code.
//! - **ProfileToneCurve** swapped in for our default tone curve when
//!   the active DCP ships one. See `crate::color::tone_curve`.
//! - **Dual-illuminant interpolation** via `illuminant.rs`:
//!   scene-temperature estimate from white-balance coefficients, linear
//!   blend between `HueSatMap1` and `HueSatMap2`.
//!
//! ## What's still deferred
//!
//! - **Forward matrix swap** (`ForwardMatrix1/2`). Our Rec.2020 path
//!   targets Rec.2020; DCP forward matrices target ProPhoto D50. A
//!   proper swap would need a full chromatic-adaptation re-pipe.
//! - **Full iterative CCT convergence.** The current dual-illuminant
//!   blend uses a one-shot WB-ratio estimate, not the spec's iterative
//!   ForwardMatrix1/2 + scene-neutral procedure. Good enough for a
//!   viewer; a later refinement.
//!
//! ## Pipeline slot
//!
//! The DCP HueSatMap runs in linear Rec.2020, **after** highlight
//! recovery and **before** the default tone curve. Same slot as
//! Lightroom's "Camera Calibration" pane: before the response shaping,
//! after the WB+matrix assembly. See `crate::decoding::raw`.

pub mod apply;
pub mod bundled;
pub mod discovery;
pub mod embedded;
pub mod family_aliases;
pub mod illuminant;
pub mod parser;

pub use apply::apply_hue_sat_map;
pub use bundled::find_bundled_dcp;
pub use discovery::{find_dcp_for_camera, log_search_summary_once};
pub use embedded::from_dng_tags;
pub use family_aliases::aliases_for;
pub use illuminant::{estimate_scene_temp_k, interpolate_hue_sat_maps};
pub use parser::Dcp;

/// Where a matched DCP came from. Logged at INFO level so users can tell
/// at a glance whether a decode picked up the DNG's own profile (almost
/// always the best source) or fell back to a filesystem copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DcpSource {
    /// Profile tags read from the DNG's own IFD. Smartphone DNGs (Pixel,
    /// Galaxy, iPhone ProRAW) and Adobe-converted DNGs ship them here.
    Embedded,
    /// Profile loaded from a standalone `.dcp` file under
    /// `$PRVW_DCP_DIR` or Adobe Camera Raw's default directory.
    Filesystem,
    /// Profile loaded from the bundled RawTherapee collection (Phase 3.5).
    /// Exact match — the camera's own profile was present in the bundle.
    Bundled,
    /// Profile loaded from the bundled collection via a fuzzy family alias
    /// (Phase 3.5). A different but sensor-compatible camera's profile is
    /// being used as a substitute. Logged at INFO so users see it.
    BundledAlias,
    /// Profile loaded from the filesystem via a fuzzy family alias (Phase
    /// 3.5). Same family-substitution logic, but the hit came from the
    /// user's own DCP directory or Adobe Camera Raw.
    FilesystemAlias,
}

impl DcpSource {
    fn label(self) -> &'static str {
        match self {
            Self::Embedded => "EMBEDDED",
            Self::Filesystem => "filesystem",
            Self::Bundled => "bundled",
            Self::BundledAlias => "bundled (alias)",
            Self::FilesystemAlias => "filesystem (alias)",
        }
    }
}

/// End-to-end "find a DCP and apply it" helper. Pass the camera's
/// `"<Make> <Model>"` identity string, an optional reference to the
/// `dng_tags` map from the decoder, the camera's `wb_coeffs` (for
/// dual-illuminant blending), and the linear-Rec.2020 buffer.
///
/// ## Source precedence (Phase 3.5)
///
/// 1. **Embedded** ([`from_dng_tags`]): the DNG's own IFD carries profile
///    tags. The camera manufacturer picked this, so it's the most
///    trustworthy source. Pixel, Samsung Galaxy, iPhone ProRAW, and
///    Adobe-converted DNGs all land here.
/// 2. **Filesystem exact** ([`find_dcp_for_camera`]): no embedded profile —
///    try a standalone `.dcp` under `$PRVW_DCP_DIR` or Adobe Camera Raw.
/// 3. **Bundled exact** ([`find_bundled_dcp`]): try the RawTherapee
///    community collection (161 profiles, gzip-packed at build time).
/// 4. **Fuzzy family alias** ([`aliases_for`]): for each alias of the
///    camera, repeat tiers 2 and 3. First hit wins. Logs at INFO so
///    users see the substitution.
/// 5. **None** — fall back to the default matrix-only pipeline.
///
/// Whichever source wins, the whole profile (HueSatMap, optional
/// LookTable, optional ProfileToneCurve) comes from that source — we
/// never mix them.
///
/// ## Pipeline order (Phase 3.4)
///
/// 1. **Dual-illuminant blend.** When the DCP ships both `HueSatMap1` and
///    `HueSatMap2` at different calibration illuminants, merge them by
///    the scene's estimated color temperature. Single-map DCPs skip this.
/// 2. **`HueSatMap` apply.** Trilinear 3D HSV LUT.
/// 3. **`LookTable` apply** (if present). Same algorithm on the same
///    buffer — Adobe's "Look" refinement on top of the neutral
///    calibration. Single-illuminant; no blending.
///
/// The profile's optional `ProfileToneCurve` is **not** applied inside
/// this function — it belongs to a later pipeline stage
/// (`color::tone_curve`). Callers read [`Dcp::tone_curve`] off the
/// returned DCP and decide whether to swap it in for our default tone
/// curve.
///
/// Returns `Some((dcp, source))` when a profile was applied, `None` when
/// nothing matched.
pub fn apply_if_available(
    camera_id: &str,
    dng_tags: Option<&std::collections::HashMap<u16, rawler::formats::tiff::Value>>,
    wb_coeffs: [f32; 4],
    rgb: &mut [f32],
) -> Option<(Dcp, DcpSource)> {
    // Prefer the embedded profile so smartphone DNGs "just work" without
    // the user installing anything. The filesystem summary is still worth
    // logging on the first call so power users see whether ACR is wired
    // up.
    log_search_summary_once();
    let embedded = if embedded_dcp_disabled() {
        None
    } else {
        dng_tags.and_then(from_dng_tags)
    };

    // Tier 1: embedded (DNG's own profile).
    // Tier 2: filesystem exact (user dir + Adobe dir).
    // Tier 3: bundled exact (RawTherapee community collection).
    let (dcp, source) = if let Some(dcp) = embedded {
        (dcp, DcpSource::Embedded)
    } else if let Some(dcp) = find_dcp_for_camera(camera_id) {
        (dcp, DcpSource::Filesystem)
    } else if let Some(dcp) = find_bundled_dcp(camera_id) {
        (dcp, DcpSource::Bundled)
    } else {
        // Tier 4: fuzzy family alias — try each alias on filesystem then
        // bundled tiers. First hit wins.
        let aliases = aliases_for(camera_id);
        if let Some((alias_dcp, alias_source)) = try_aliases(camera_id, aliases) {
            (alias_dcp, alias_source)
        } else {
            return None;
        }
    };
    let scene_temp_k = estimate_scene_temp_k(wb_coeffs);
    if let Some(map) = interpolate_hue_sat_maps(&dcp, scene_temp_k) {
        let blended = dcp.hue_sat_map_1.is_some()
            && dcp.hue_sat_map_2.is_some()
            && dcp.calibration_illuminant_1 != dcp.calibration_illuminant_2;
        if blended {
            log::debug!(
                "DCP dual-illuminant blend at scene temp {:.0} K",
                scene_temp_k
            );
        }
        apply_hue_sat_map(rgb, &map, dcp.hue_sat_map_encoding);
    }
    if let Some(look) = &dcp.look_table {
        log::debug!(
            "DCP LookTable apply ({}×{}×{})",
            look.hue_divs,
            look.sat_divs,
            look.val_divs
        );
        apply_hue_sat_map(rgb, look, dcp.look_table_encoding);
    }
    Some((dcp, source))
}

/// Try each alias for a camera across the filesystem and bundled tiers.
///
/// Returns the first matching `(Dcp, DcpSource)` where `DcpSource` is
/// `FilesystemAlias` or `BundledAlias`. Returns `None` if none of the
/// aliases yield a hit.
fn try_aliases(camera_id: &str, aliases: &[&str]) -> Option<(Dcp, DcpSource)> {
    for alias in aliases {
        if let Some(dcp) = find_dcp_for_camera(alias) {
            log::info!(
                "DCP: no exact match for '{}'; using compatible profile '{}' from filesystem",
                camera_id,
                dcp.profile_name
                    .as_deref()
                    .or(dcp.unique_camera_model.as_deref())
                    .unwrap_or(*alias)
            );
            return Some((dcp, DcpSource::FilesystemAlias));
        }
        if let Some(dcp) = find_bundled_dcp(alias) {
            log::info!(
                "DCP: no exact match for '{}'; using compatible profile '{}' from bundled collection",
                camera_id,
                dcp.profile_name
                    .as_deref()
                    .or(dcp.unique_camera_model.as_deref())
                    .unwrap_or(*alias)
            );
            return Some((dcp, DcpSource::BundledAlias));
        }
    }
    None
}

/// Env var that lets the embedded-DCP smoke test and power users force
/// the pipeline to ignore DNG-embedded profile tags. Set to `1` (or any
/// non-empty value) to skip the embedded-profile read and fall through
/// to filesystem discovery + the default pipeline.
pub const EMBEDDED_DCP_DISABLE_ENV_VAR: &str = "PRVW_DISABLE_EMBEDDED_DCP";

fn embedded_dcp_disabled() -> bool {
    std::env::var_os(EMBEDDED_DCP_DISABLE_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Human-readable label for the DCP source. Used by the decoder so the
/// INFO log line always spells out which code path produced the profile.
pub fn source_label(source: DcpSource) -> &'static str {
    source.label()
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
