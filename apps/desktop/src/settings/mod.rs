//! Settings: JSON persistence + the Settings window UI shell.
//!
//! Persistence lives in `persistence` (load/save `Settings` struct).
//! The window UI is orchestrated by `window` — it assembles per-feature panels
//! (color, zoom, file associations) plus the cross-feature "General" panel.

mod panels;
pub mod persistence;
pub mod widgets;
mod window;

pub use persistence::Settings;
pub use window::{close_settings_window, show_settings_window, switch_settings_section};
