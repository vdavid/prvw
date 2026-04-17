//! Settings: JSON persistence + the Settings window UI shell.
//!
//! Persistence lives in `persistence` (load/save `Settings` struct) and is
//! platform-independent. The window UI (`window`, `widgets`, `panels`) is
//! macOS-only (AppKit) — other platforms will get their own UI shell later.

#[cfg(target_os = "macos")]
mod panels;
pub mod persistence;
#[cfg(target_os = "macos")]
pub mod widgets;
#[cfg(target_os = "macos")]
mod window;

pub use persistence::Settings;
#[cfg(target_os = "macos")]
pub use window::{close_settings_window, show_settings_window, switch_settings_section};
