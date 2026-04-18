//! `RawPipelineFlags` — per-step toggles and tuning knobs for the RAW decode
//! pipeline.
//!
//! The defaults reproduce today's pipeline bit-for-bit. Each flag guards a
//! single stage in `decoding::raw::decode`; flipping one off lets users and
//! developers see what that stage actually contributes to the final image.
//! Phase 6.0 added three float-valued tuning knobs alongside the flags
//! (`sharpen_amount`, `saturation_boost_amount`, `midtone_anchor`) so users
//! can dial in the parametric stages by eye from the Settings → RAW panel.
//! Driven by the Settings → RAW panel (see `settings/panels/raw.rs`) and by
//! the JSON persistence layer (`settings::persistence::Settings::raw`).
//!
//! See `docs/notes/raw-support-phase3.md` (Phase 3.7) and
//! `docs/notes/raw-support-phase6.md` (Phase 6.0) for rationale and intended
//! use.

use serde::{Deserialize, Serialize};

use crate::color::saturation::DEFAULT_SATURATION_BOOST;
use crate::color::sharpen::DEFAULT_AMOUNT;
use crate::color::tone_curve::DEFAULT_MIDTONE_ANCHOR;

/// Valid range for the unsharp-mask amount slider. 0.0 = no sharpening,
/// 1.0 = hard edges / halos — users who want more can bump it but we cap
/// here to keep the knob useful.
pub const SHARPEN_AMOUNT_RANGE: (f32, f32) = (0.0, 1.0);
/// Valid range for the saturation boost slider. 0.0 = untouched, 0.30 = the
/// "too vibrant" end where skin tones start to push. Negative values
/// desaturate but we don't expose those — users who want less chroma usually
/// reach for HueSatMap or a custom DCP.
pub const SATURATION_BOOST_RANGE: (f32, f32) = (0.0, 0.30);
/// Valid range for the tone-curve midtone anchor. 0.20 crushes midtones
/// (darker overall), 0.50 lifts them hard (brighter overall). The default
/// (0.40) lands between Adobe Linear and Medium Contrast.
pub const MIDTONE_ANCHOR_RANGE: (f32, f32) = (0.20, 0.50);

/// Per-stage flags + tuning knobs for the RAW pipeline. All `true` and
/// defaults-at-constants = production behavior.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RawPipelineFlags {
    // ── Sensor corrections (DNG only — no-op on ARW, CR2, NEF, etc.) ───
    #[serde(default = "default_true")]
    pub dng_opcode_list_1: bool,
    #[serde(default = "default_true")]
    pub dng_opcode_list_2: bool,
    #[serde(default = "default_true")]
    pub dng_opcode_list_3: bool,

    // ── Color ──────────────────────────────────────────────────────────
    #[serde(default = "default_true")]
    pub baseline_exposure: bool,
    #[serde(default = "default_true")]
    pub dcp_hue_sat_map: bool,
    #[serde(default = "default_true")]
    pub dcp_look_table: bool,
    #[serde(default = "default_true")]
    pub saturation_boost: bool,

    // ── Tone ───────────────────────────────────────────────────────────
    #[serde(default = "default_true")]
    pub highlight_recovery: bool,
    /// Gates Prvw's default Hermite S-curve. `#[serde(alias = "tone_curve")]`
    /// keeps older settings.json files (pre-Phase 3.7.1) still reading their
    /// persisted value into this field after the rename.
    #[serde(default = "default_true", alias = "tone_curve")]
    pub default_tone_curve: bool,
    /// Gates the DCP's own `ProfileToneCurve` when an active profile ships
    /// one. Independent of `default_tone_curve`; both may be on, off, or
    /// mixed. **Additionally**, DCP tone curves are auto-skipped when the
    /// profile was matched via a fuzzy family alias (e.g., an α6000 curve
    /// applied to an α5000) because the camera-maker's tonality target
    /// doesn't transfer cleanly across bodies. Logs spell out which curve
    /// ran and why.
    #[serde(default = "default_true")]
    pub dcp_tone_curve: bool,

    // ── Detail ─────────────────────────────────────────────────────────
    #[serde(default = "default_true")]
    pub capture_sharpening: bool,

    // ── Geometry ──────────────────────────────────────────────────────
    /// Lens distortion, TCA, and vignetting correction via `lensfun-rs`.
    /// Fires post-demosaic, pre-`camera_to_linear_rec2020`. Silent no-op
    /// when rawler exposes no lens model or the lens isn't in LensFun's
    /// database. DNGs whose `OpcodeList3` already ran `WarpRectilinear`
    /// are skipped (see `decoding::raw::decode`) to avoid double
    /// correction.
    #[serde(default = "default_true")]
    pub lens_correction: bool,

    // ── Output (Phase 5) ──────────────────────────────────────────────
    /// HDR / EDR output: keep highlights above display-white alive through
    /// the tone curve and ship a `RGBA16F` buffer to the renderer so an
    /// EDR-capable display can show them. Silent no-op on SDR-only displays
    /// (the headroom query returns 1.0 and the f16 conversion is the same
    /// [0, 1] clamp Phase 4 produced). Defaults to `true`; users on mini-LED
    /// or OLED XDR displays get HDR highlights by default, and the toggle
    /// in Settings → RAW → Output lets them opt out.
    #[serde(default = "default_true")]
    pub hdr_output: bool,

    // ── Tuning knobs (Phase 6.0) ──────────────────────────────────────
    /// Unsharp-mask amount for capture sharpening. Threaded through
    /// `color::sharpen::sharpen_rgba8_inplace_with` /
    /// `sharpen_rgba16f_inplace_with`. Defaults to
    /// [`color::sharpen::DEFAULT_AMOUNT`]. Range:
    /// [`SHARPEN_AMOUNT_RANGE`].
    #[serde(default = "default_sharpen_amount")]
    pub sharpen_amount: f32,
    /// Global saturation boost in linear Rec.2020. Threaded through
    /// `color::saturation::apply_saturation_boost`. Defaults to
    /// [`color::saturation::DEFAULT_SATURATION_BOOST`]. Range:
    /// [`SATURATION_BOOST_RANGE`].
    #[serde(default = "default_saturation_boost_amount")]
    pub saturation_boost_amount: f32,
    /// Midtone anchor for the default tone curve. The point at which the
    /// midtone line passes through `(x, x)`. Threaded through
    /// `color::tone_curve::apply_tone_curve`. Defaults to
    /// [`color::tone_curve::DEFAULT_MIDTONE_ANCHOR`]. Range:
    /// [`MIDTONE_ANCHOR_RANGE`].
    #[serde(default = "default_midtone_anchor")]
    pub midtone_anchor: f32,
}

fn default_true() -> bool {
    true
}

fn default_sharpen_amount() -> f32 {
    DEFAULT_AMOUNT
}

fn default_saturation_boost_amount() -> f32 {
    DEFAULT_SATURATION_BOOST
}

fn default_midtone_anchor() -> f32 {
    DEFAULT_MIDTONE_ANCHOR
}

impl Default for RawPipelineFlags {
    fn default() -> Self {
        Self {
            dng_opcode_list_1: true,
            dng_opcode_list_2: true,
            dng_opcode_list_3: true,
            baseline_exposure: true,
            dcp_hue_sat_map: true,
            dcp_look_table: true,
            saturation_boost: true,
            highlight_recovery: true,
            default_tone_curve: true,
            dcp_tone_curve: true,
            capture_sharpening: true,
            lens_correction: true,
            hdr_output: true,
            sharpen_amount: DEFAULT_AMOUNT,
            saturation_boost_amount: DEFAULT_SATURATION_BOOST,
            midtone_anchor: DEFAULT_MIDTONE_ANCHOR,
        }
    }
}

impl RawPipelineFlags {
    /// True when every flag and knob is at its production default. The
    /// decoder uses this to stay silent on the hot path and log a
    /// diagnostic breadcrumb only when the user has changed something.
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Clamp each tuning knob into its valid range. Protects against
    /// malformed `settings.json` values (the user might hand-edit the
    /// file) without rejecting the whole file. Called once per decode by
    /// `decoding::raw`.
    pub fn clamp_knobs(&mut self) {
        self.sharpen_amount = self
            .sharpen_amount
            .clamp(SHARPEN_AMOUNT_RANGE.0, SHARPEN_AMOUNT_RANGE.1);
        self.saturation_boost_amount = self
            .saturation_boost_amount
            .clamp(SATURATION_BOOST_RANGE.0, SATURATION_BOOST_RANGE.1);
        self.midtone_anchor = self
            .midtone_anchor
            .clamp(MIDTONE_ANCHOR_RANGE.0, MIDTONE_ANCHOR_RANGE.1);
    }

    /// Names of the disabled steps, in the order they appear in the
    /// pipeline and the Settings panel. Used for the INFO log line when any
    /// flag is non-default.
    pub fn disabled_step_labels(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.dng_opcode_list_1 {
            out.push("DNG OpcodeList 1");
        }
        if !self.dng_opcode_list_2 {
            out.push("DNG OpcodeList 2");
        }
        if !self.dng_opcode_list_3 {
            out.push("DNG OpcodeList 3");
        }
        if !self.baseline_exposure {
            out.push("baseline exposure");
        }
        if !self.highlight_recovery {
            out.push("highlight recovery");
        }
        if !self.dcp_hue_sat_map {
            out.push("DCP HueSatMap");
        }
        if !self.dcp_look_table {
            out.push("DCP LookTable");
        }
        if !self.default_tone_curve {
            out.push("default tone curve");
        }
        if !self.dcp_tone_curve {
            out.push("DCP tone curve");
        }
        if !self.saturation_boost {
            out.push("saturation boost");
        }
        if !self.capture_sharpening {
            out.push("capture sharpening");
        }
        if !self.lens_correction {
            out.push("lens correction");
        }
        if !self.hdr_output {
            out.push("HDR output");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_all_true_and_knobs_at_constants() {
        let flags = RawPipelineFlags::default();
        assert!(flags.dng_opcode_list_1);
        assert!(flags.dng_opcode_list_2);
        assert!(flags.dng_opcode_list_3);
        assert!(flags.baseline_exposure);
        assert!(flags.dcp_hue_sat_map);
        assert!(flags.dcp_look_table);
        assert!(flags.saturation_boost);
        assert!(flags.highlight_recovery);
        assert!(flags.default_tone_curve);
        assert!(flags.dcp_tone_curve);
        assert!(flags.capture_sharpening);
        assert!(flags.lens_correction);
        assert!(flags.hdr_output);
        assert_eq!(flags.sharpen_amount, DEFAULT_AMOUNT);
        assert_eq!(flags.saturation_boost_amount, DEFAULT_SATURATION_BOOST);
        assert_eq!(flags.midtone_anchor, DEFAULT_MIDTONE_ANCHOR);
        assert!(flags.is_default());
        assert!(flags.disabled_step_labels().is_empty());
    }

    #[test]
    fn round_trip_preserves_values() {
        let flags = RawPipelineFlags {
            highlight_recovery: false,
            capture_sharpening: false,
            sharpen_amount: 0.55,
            saturation_boost_amount: 0.12,
            midtone_anchor: 0.33,
            ..RawPipelineFlags::default()
        };
        let json = serde_json::to_string(&flags).unwrap();
        let decoded: RawPipelineFlags = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, flags);
        assert!(!decoded.is_default());
    }

    #[test]
    fn round_trip_preserves_float_precision() {
        // Serde's float precision should round-trip the exact bit pattern
        // for reasonable values. Using a non-round value catches any
        // accidental truncation in the persistence path.
        let flags = RawPipelineFlags {
            sharpen_amount: 0.375,
            saturation_boost_amount: 0.125,
            midtone_anchor: 0.425,
            ..RawPipelineFlags::default()
        };
        let json = serde_json::to_string(&flags).unwrap();
        let decoded: RawPipelineFlags = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.sharpen_amount, 0.375);
        assert_eq!(decoded.saturation_boost_amount, 0.125);
        assert_eq!(decoded.midtone_anchor, 0.425);
    }

    #[test]
    fn missing_fields_default() {
        // Simulate an old settings.json that didn't have the RAW block at all.
        let decoded: RawPipelineFlags = serde_json::from_str("{}").unwrap();
        assert!(decoded.is_default());
        assert_eq!(decoded.sharpen_amount, DEFAULT_AMOUNT);
        assert_eq!(decoded.saturation_boost_amount, DEFAULT_SATURATION_BOOST);
        assert_eq!(decoded.midtone_anchor, DEFAULT_MIDTONE_ANCHOR);
    }

    #[test]
    fn clamp_knobs_pulls_out_of_range_values_into_range() {
        let mut flags = RawPipelineFlags {
            sharpen_amount: 9.0,
            saturation_boost_amount: -0.5,
            midtone_anchor: 0.90,
            ..RawPipelineFlags::default()
        };
        flags.clamp_knobs();
        assert_eq!(flags.sharpen_amount, SHARPEN_AMOUNT_RANGE.1);
        assert_eq!(flags.saturation_boost_amount, SATURATION_BOOST_RANGE.0);
        assert_eq!(flags.midtone_anchor, MIDTONE_ANCHOR_RANGE.1);
    }

    #[test]
    fn clamp_knobs_preserves_in_range_values() {
        let mut flags = RawPipelineFlags {
            sharpen_amount: 0.42,
            saturation_boost_amount: 0.15,
            midtone_anchor: 0.30,
            ..RawPipelineFlags::default()
        };
        flags.clamp_knobs();
        assert_eq!(flags.sharpen_amount, 0.42);
        assert_eq!(flags.saturation_boost_amount, 0.15);
        assert_eq!(flags.midtone_anchor, 0.30);
    }

    #[test]
    fn disabled_labels_ordered() {
        let flags = RawPipelineFlags {
            default_tone_curve: false,
            capture_sharpening: false,
            ..RawPipelineFlags::default()
        };
        assert_eq!(
            flags.disabled_step_labels(),
            vec!["default tone curve", "capture sharpening"]
        );
    }

    #[test]
    fn serde_alias_migrates_old_tone_curve_field() {
        // Old settings.json had `tone_curve: false`. Confirm it lands on the
        // renamed `default_tone_curve` field so users don't lose their
        // preference after the 3.7.1 rename.
        let decoded: RawPipelineFlags = serde_json::from_str(r#"{"tone_curve": false}"#).unwrap();
        assert!(!decoded.default_tone_curve);
        assert!(decoded.dcp_tone_curve);
    }
}
