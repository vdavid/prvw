//! Settings: JSON persistence, plus one settings window per platform.
//!
//! `persistence` owns the `Settings` struct and its JSON file, and it's the same on every
//! platform. The UI over it forks: `window` / `widgets` / `panels` are the macOS AppKit
//! window, and `windows` is the Win32 dialog. They share the model and nothing else, which is
//! decision 1b in `docs/specs/cross-platform-plan.md`. Linux has neither yet.
//!
//! The `windows` module's pure half (its model, layout, and file-type registration) compiles
//! everywhere on purpose, so a Mac's `cargo test` checks what a Windows user will see.

#[cfg(target_os = "macos")]
mod panels;
pub mod persistence;
#[cfg(target_os = "macos")]
pub mod widgets;
#[cfg(target_os = "macos")]
mod window;
// The Windows dialog's model, layout, and file-type registration compile on every platform so
// a Mac can test them, but only a Windows build has a dialog to consume them. macOS is the
// host that runs their tests, so nothing here rots unnoticed.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub mod windows;

pub use persistence::Settings;
#[cfg(target_os = "macos")]
pub use window::{close_settings_window, show_settings_window, switch_settings_section};
#[cfg(target_os = "windows")]
pub use windows::{
    close_settings_window, show_settings_window, switch_settings_section, sync_custom_dcp_dir,
};
