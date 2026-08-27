//! Browse mode on Windows: a folder tree, a thumbnail grid, a splitter, and a status bar.
//!
//! Explorer's shape and ACDSee's, which is what a Windows user will compare it to. It is
//! deliberately **not** a port of the macOS browser: no sidebar vibrancy, no rounded gallery
//! surface, no insets for traffic lights, and a status bar macOS doesn't have. The reasoning is
//! `docs/specs/windows-ui-design.md` → "Browse mode", and the decision behind it is David's:
//! "make the Windows version very Windows-like".
//!
//! What carries over unchanged is everything underneath the widgets. `grid_model`,
//! `grid_scheduler`, `thumbnail_cache`, `tree_model`, and `grid_listing` are platform-free and
//! already tested; this module is the shell around them, the same way `split_view`, `outline`,
//! and `grid` are on macOS.
//!
//! ## The split, and why it's here
//!
//! Every module here compiles on every platform and is tested on a Mac. The Win32 layer that
//! consumes them decides as little as it can, which is the split that made the settings dialog
//! land in one pass (`settings::windows`).
//!
//! - [`roots`] is what the tree shows at its top level: known folders, drive letters, their
//!   labels, and which entries are too hidden to list.
//! - [`layout`] is where the four children go, in device pixels at the monitor's own DPI.
//! - [`status`] is what the status bar says.
//! - [`keys`] is the three keys the panes take and the many they leave to the controls.

pub mod keys;
pub mod layout;
pub mod roots;
pub mod status;
