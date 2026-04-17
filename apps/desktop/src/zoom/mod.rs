//! Zoom & pan: view state, cursor-centered zoom, fit/actual modes.
//!
//! The actual renderer lives under `crate::render`; this feature contributes the
//! `ViewState` math, the zoom-related `AppCommand` handling, and its Settings panel.

#[cfg(target_os = "macos")]
pub mod settings_panel;
pub mod view;

use crate::settings::Settings;

/// Per-feature runtime state owned by `App`.
pub struct State {
    /// Whether the window auto-resizes to fit each loaded image.
    pub auto_fit: bool,
    /// Whether small images are enlarged to fill the window.
    pub enlarge: bool,
    /// Whether scroll wheel/touchpad zooms (true) or navigates images (false).
    pub scroll_to_zoom: bool,
    /// Zoom/pan math + transform uniform.
    pub view: view::ViewState,
}

impl State {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            auto_fit: settings.auto_fit_window,
            enlarge: settings.enlarge_small_images,
            scroll_to_zoom: settings.scroll_to_zoom,
            view: view::ViewState::new(),
        }
    }
}
