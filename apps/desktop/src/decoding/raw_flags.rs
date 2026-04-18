//! `RawPipelineFlags` — per-step toggles for the RAW decode pipeline.
//!
//! The defaults reproduce today's pipeline bit-for-bit. Each flag guards a
//! single stage in `decoding::raw::decode`; flipping one off lets users and
//! developers see what that stage actually contributes to the final image.
//! Driven by the Settings → RAW panel (see `settings/panels/raw.rs`) and by
//! the JSON persistence layer (`settings::persistence::Settings::raw`).
//!
//! See `docs/notes/raw-support-phase3.md` (Phase 3.7) for rationale and
//! intended use.

use serde::{Deserialize, Serialize};

/// One boolean per RAW pipeline stage. All `true` = production behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
}

fn default_true() -> bool {
    true
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
        }
    }
}

impl RawPipelineFlags {
    /// True when every flag is at its production default. The decoder uses
    /// this to stay silent on the hot path and log a diagnostic breadcrumb
    /// only when the user has flipped something off.
    pub fn is_default(&self) -> bool {
        self == &Self::default()
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
    fn defaults_are_all_true() {
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
        assert!(flags.is_default());
        assert!(flags.disabled_step_labels().is_empty());
    }

    #[test]
    fn round_trip_preserves_values() {
        let flags = RawPipelineFlags {
            highlight_recovery: false,
            capture_sharpening: false,
            ..RawPipelineFlags::default()
        };
        let json = serde_json::to_string(&flags).unwrap();
        let decoded: RawPipelineFlags = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, flags);
        assert!(!decoded.is_default());
    }

    #[test]
    fn missing_fields_default_to_true() {
        // Simulate an old settings.json that didn't have the RAW block at all.
        let decoded: RawPipelineFlags = serde_json::from_str("{}").unwrap();
        assert!(decoded.is_default());
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
