//! The Windows settings dialog.
//!
//! A modeless, non-blocking dialog holding a `SysTabControl32` with six tabs and one Close
//! button, built from Win32 common controls through the `windows` crate. The main window stays
//! live behind it, which is the rule the whole design turns on: a Win32 modal loop doesn't
//! crash the way an AppKit modal does, it starves winit's pump, and the slideshow timer stops
//! with it (`platform::windows::msg_hook`).
//!
//! It is deliberately not a port of the macOS window. Windows gets checkboxes rather than
//! switches, trackbars rather than sliders, group boxes rather than bold section headers, and
//! tabs rather than a sidebar, because each of those is what the platform's own settings
//! dialogs look like (`docs/specs/windows-ui-design.md`). What the two share is the model
//! underneath: one `Settings` struct, one `SettingKey` registry, one `AppCommand` path in.
//!
//! ## The split, and why it's here
//!
//! - [`model`] is what the dialog holds: tabs, rows, copy, trackbar ranges, and the rule that
//!   turns a control's new value into an `AppCommand`.
//! - [`layout`] is where every control goes, in device pixels at the monitor's own DPI.
//!   Parameterised by a text measurer, so the wrapping rule can be checked without GDI.
//! - [`file_types`] is what the File associations page writes into the registry, worked out as
//!   data before anything touches `HKEY_CURRENT_USER`.
//! - [`ids`] maps a control's integer id back to the row and the part it belongs to, and
//!   [`template`] builds the `DLGTEMPLATE` bytes the dialog is created from.
//! - `dialog` is the Win32 layer, and the only part a Mac can't run: it walks the model,
//!   creates one control per row, and pumps `WM_COMMAND`, `WM_HSCROLL`, and `WM_VSCROLL` back
//!   through [`model::apply`].
//!
//! Painting isn't here either: `crate::chrome` holds the colour policy for every Win32 window
//! Prvw puts up, this dialog and the About box alike, and `platform::windows::dark_mode` calls
//! Win32 with it. `chrome` compiles everywhere, so which colour a control gets is asserted on a
//! Mac rather than eyeballed on a Windows box.
//!
//! Everything but `dialog` compiles on every platform and is tested on macOS. Nothing here has
//! ever executed on Windows, so that's where the correctness lives, and the FFI layer is kept
//! as thin as it can be.

pub mod file_types;
pub mod ids;
pub mod layout;
pub mod model;
pub mod template;

#[cfg(target_os = "windows")]
mod dialog;

#[cfg(target_os = "windows")]
pub use dialog::{
    close_settings_window, show_settings_window, switch_settings_section, sync_custom_dcp_dir,
};
