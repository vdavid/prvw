//! A platform that handles one entry from each registry and forgets the rest.
//!
//! This file is meant NOT to compile: `tests/parity_registries.rs` runs `rustc` over it and
//! asserts it fails. That failure is the whole point of the parity harness, so it's worth
//! having a test that watches it happen rather than trusting that it would.
//!
//! It lives under `tests/parity_fixtures/`, which Cargo doesn't build as a test target, so
//! nothing else ever compiles it.

#[path = "../../src/parity/mod.rs"]
mod parity;

use parity::command_keys::CommandKey;
use parity::menu_items::MenuItemKey;
use parity::setting_keys::SettingKey;

/// What a new platform's settings-panel builder looks like on day one.
pub fn build_settings_row(key: SettingKey) -> &'static str {
    match key {
        SettingKey::AutoUpdate => "a checkbox",
    }
}

/// The same for its menu bar.
pub fn build_menu_item(key: MenuItemKey) -> &'static str {
    match key {
        MenuItemKey::About => "an item",
    }
}

/// And its command dispatcher.
pub fn dispatch(key: CommandKey) -> &'static str {
    match key {
        CommandKey::ZoomIn => "zoomed",
    }
}
