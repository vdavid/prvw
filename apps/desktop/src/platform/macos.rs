//! macOS-specific integrations: display color profiles, Finder file opens, file
//! associations, auto-update, and AppKit secondary windows.
//!
//! Everything in this subtree is gated behind `#[cfg(target_os = "macos")]` at the
//! `crate::platform::macos` import site. Callers outside this module should reference
//! these via `crate::platform::macos::{display_profile, native_ui, ...}`.

pub mod display_profile;
pub mod file_associations;
pub mod native_ui;
pub mod open_handler;
pub mod updater;
