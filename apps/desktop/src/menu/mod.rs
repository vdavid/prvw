//! The menu bar and the right-click context menu.
//!
//! ## The platform seam
//!
//! A native menu bar is not a given. macOS and Windows both have one and `muda` drives both;
//! Linux has no menu bar Prvw can attach to, because muda only offers `init_for_gtk_window`
//! there and winit can't hand it a `gtk::Window` (see `docs/specs/cross-platform-plan.md`).
//! So muda is a macOS-and-Windows dependency, and this module has two implementations behind
//! one API:
//!
//! - [`native`] builds the real thing with muda.
//! - [`absent`] is the platform that has no menu bar. [`AppMenu`] there is uninhabited and
//!   [`create_menu_bar`] returns `None`.
//!
//! `App` holds an `Option<AppMenu>` (it's `None` before `resumed()` builds it, and forever on a
//! platform with no menu bar) and talks to it only through the five methods below. That's what
//! keeps `#[cfg]` out of `app.rs` and `app/executor.rs` entirely: the call sites are the same
//! on every platform, and the menu-less build simply never has a menu to call.
//!
//! ## One item set, two sets of decoration
//!
//! What the menus contain is shared: `parity::menu_items` names every item once and supplies
//! the one label the product calls it by. What an item *looks like* is not: macOS pads a
//! cosmetic shortcut hint into the title and binds Command; Windows marks a mnemonic with `&`,
//! right-aligns a tab-separated shortcut column, and binds Ctrl. [`macos`] and [`windows`] are
//! those two tables, `native` picks one as `chrome`, and neither ever renames a shared label.
//!
//! Both tables compile on both platforms, deliberately, for the same reason the parity
//! registries do: a `cargo test` on a Mac runs the Windows table's tests, and a Windows build
//! type-checks the macOS one. Only the code that touches Win32 is `#[cfg]`-gated.
//!
//! ## State flows one way
//!
//! Settings are the source of truth for every checkmark and every enabled/disabled item.
//! [`AppMenu::sync_from_settings`] is the single, idempotent place that maps them onto the
//! menu, and `create_menu_bar` calls it too, so initial state and later state can't drift.
//! Commands run the other way, through [`AppMenu::poll_command`]. Nothing else pokes menu items.
//!
//! ## What Linux loses
//!
//! Actions the menu is the only route to (Sort by, Auto-fit window, Enlarge small images, the
//! three ICC toggles, Refresh, Start/stop slideshow) are unreachable there. That's the decided
//! scope, not an oversight: see `menu/CLAUDE.md` for the full reachability list.

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use native::{AppMenu, create_menu_bar};

// The two chrome tables. Each is dead code on the platform that isn't using it, which is the
// price of having one host check the other's table.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod macos;
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod windows;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod absent;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use absent::{AppMenu, create_menu_bar};
