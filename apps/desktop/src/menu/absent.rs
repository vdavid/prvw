//! The menu for a platform that has no native menu bar (Linux today).
//!
//! [`AppMenu`] is uninhabited on purpose. `create_menu_bar` always answers `None`, so no value
//! of this type can ever exist and every method below is unreachable, which is exactly the
//! claim we want the compiler to check. Mirroring the native API here is what lets `app.rs` and
//! `app/executor.rs` call into the menu with no `#[cfg]` of their own.

use crate::commands::AppCommand;
use crate::settings::Settings;

/// The menu bar on a platform that has none. See the module docs for why it's uninhabited.
pub enum AppMenu {}

impl AppMenu {
    /// Push settings onto the menu's checkmarks and enabled states.
    pub fn sync_from_settings(&self, _settings: &Settings) {
        match *self {}
    }

    /// Flip the Slideshow menu's first item between "Start slideshow" and "Stop slideshow".
    pub fn set_slideshow_running(&self, _running: bool) {
        match *self {}
    }

    /// Flip the Navigate menu's first item between "Image browser" and "Image view".
    pub fn set_browse_mode(&self, _browsing: bool) {
        match *self {}
    }

    /// Take the bar away for fullscreen, and put it back on the way out.
    pub fn set_fullscreen(&self, _fullscreen: bool) {
        match *self {}
    }

    /// Take the next pending menu click, if any, as an `AppCommand`.
    pub fn poll_command(&self) -> Option<AppCommand> {
        match *self {}
    }
}

/// No menu bar to build. Every menu-only action is unreachable here; the module docs and
/// `menu/CLAUDE.md` list which ones and why that's the decided scope.
pub fn create_menu_bar(_window: &winit::window::Window) -> Option<AppMenu> {
    log::debug!("No native menu bar on this platform; menu-only actions are unavailable");
    None
}
