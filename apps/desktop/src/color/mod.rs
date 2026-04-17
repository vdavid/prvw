//! Color management: ICC profile extraction, transform, display profile detection,
//! Settings panel.

pub mod delta_e;
pub mod profiles;
mod transform;

pub use profiles::linear_rec2020_profile;
pub use transform::{profiles_match, srgb_icc_bytes, transform_f32_with_profile, transform_icc};

#[cfg(target_os = "macos")]
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
    /// ICC bytes for the target display. Defaults to sRGB; updated when the display
    /// is detected or the window moves.
    pub display_icc: Vec<u8>,
}

impl State {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            icc_enabled: settings.icc_color_management,
            match_display: settings.color_match_display,
            relative_col: settings.use_relative_colorimetric,
            display_icc: srgb_icc_bytes().to_vec(),
        }
    }
}
