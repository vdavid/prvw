//! The control for `misses_an_arm.rs`: the same registries, consumed the way that compiles.
//!
//! Without this, a fixture that failed for an unrelated reason (a typo, a module that won't
//! load) would still look like proof. `tests/parity_registries.rs` compiles this one and
//! asserts it succeeds.

#[path = "../../src/parity/mod.rs"]
mod parity;

use parity::setting_keys::SettingKey;
use parity::{Coverage, Platform};

/// A platform's answer for one entry. `NotApplicable` is a first-class answer, and the reason
/// travels with it as data rather than as a comment: layer 2 renders it in the parity table.
pub fn coverage(key: SettingKey) -> Coverage {
    match key {
        SettingKey::TitleBar => Coverage::NotApplicable {
            reason: "This platform's windows don't draw content behind their title bar.",
        },
        // Every other key answers through the registry's own declaration, which is what a real
        // platform builder is checked against.
        other => other.coverage(Platform::MacOs),
    }
}
