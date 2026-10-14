//! Color management: ICC profile extraction, transform, display profile detection,
//! Settings panel.

pub mod chroma_denoise;
pub mod clarity;
pub mod dcp;
pub mod delta_e;
pub mod display_sdr;
pub mod hdr;
pub mod highlight_recovery;
pub mod lens_correction;
pub mod profiles;
pub mod saturation;
pub mod sharpen;
pub mod tone_curve;
mod transform;

pub use profiles::linear_rec2020_profile;
#[cfg(test)]
pub(crate) use transform::lut_only_icc;
pub use transform::{
    display_p3_icc_bytes, profiles_match, srgb_icc_bytes, transform_icc, transform_icc_16bit,
    transform_linear_f32_to_display,
};

pub mod display_profile;
#[cfg(target_os = "macos")]
pub mod settings_panel;

use crate::settings::Settings;

/// Per-feature runtime state owned by `App`.
pub struct State {
    /// ICC color management (Level 1: source → sRGB when match_display is off).
    pub icc_enabled: bool,
    /// Level 2: target is the display profile (when on) or sRGB (when off).
    pub match_display: bool,
    /// Relative colorimetric rendering intent (when on) vs perceptual (when off).
    pub relative_col: bool,
    /// "HDR output": HDR images show brighter than SDR white when the display can. Off, every
    /// image renders SDR. `App::hdr_active` combines it with the display's capability.
    pub hdr_output: bool,
    /// ICC bytes for the target display. Defaults to sRGB; updated when the display
    /// is detected or the window moves.
    pub display_icc: Vec<u8>,
    /// Which display `display_icc` was read for, so a window drag doesn't re-read a profile per
    /// mouse move. Only the platforms that learn about a screen change from the move itself
    /// consult it; see [`display_profile::MonitorTracker`].
    pub monitors: display_profile::MonitorTracker,
}

impl State {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            icc_enabled: settings.icc_color_management,
            match_display: settings.color_match_display,
            relative_col: settings.use_relative_colorimetric,
            hdr_output: settings.color.hdr_output,
            display_icc: srgb_icc_bytes().to_vec(),
            monitors: display_profile::MonitorTracker::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `App::new` builds this on every platform, and it used to reach for a
    /// macOS-only system file to do it, so the app died at startup elsewhere.
    /// The target profile now comes from `moxcms`, so this touches no disk.
    #[test]
    fn state_builds_without_reading_the_filesystem() {
        let state = State::from_settings(&Settings::default());
        assert!(
            !state.display_icc.is_empty(),
            "the default target profile must have bytes"
        );
        moxcms::ColorProfile::new_from_slice(&state.display_icc)
            .expect("the default target profile must parse");
    }
}
