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
//! platform with no menu bar) and talks to it only through the four methods below. That's what
//! keeps `#[cfg]` out of `app.rs` and `app/executor.rs` entirely: the call sites are the same
//! on every platform, and the menu-less build simply never has a menu to call.
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

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod absent;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use absent::{AppMenu, create_menu_bar};
