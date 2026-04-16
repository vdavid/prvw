//! Platform-specific integrations. Today, macOS only.
//!
//! Per-platform submodules live under `platform::<os>` and are gated with `#[cfg]`.
//! When a second platform lands, mirror the `macos/` shape with its own submodule.

#[cfg(target_os = "macos")]
pub mod macos;
