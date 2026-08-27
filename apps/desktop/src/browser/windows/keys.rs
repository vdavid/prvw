//! The three keys the browse panes take off Windows, and the many they leave alone.
//!
//! The twin of `browser::browse_keydown_command`, which does this for macOS hardware key codes.
//! Same rule on both platforms, and it is the rule that keeps the browser feeling native: the
//! tree and the grid handle their own arrows, page keys, Home, End, and type-select, because
//! `SysTreeView32` and `SysListView32` already do all of that better than we would. We intercept
//! Tab, Enter, and Esc, and nothing else.
//!
//! Backspace is deliberately absent. Explorer's convention is that it goes to the parent folder,
//! and the tree honours that — but by walking to the parent node itself, with no command in
//! between, exactly as arrow keys do.

use crate::commands::AppCommand;

/// Virtual-key codes, from `winuser.h`. Named rather than pulled from the `windows` crate so
/// this module compiles on every host and the mapping is testable from a Mac.
pub mod vk {
    /// `VK_BACK`.
    pub const BACK: u32 = 0x08;
    /// `VK_TAB`.
    pub const TAB: u32 = 0x09;
    /// `VK_RETURN` — both the main Return key and the numeric keypad's Enter, which Windows
    /// distinguishes by an extended-key flag rather than by code.
    pub const RETURN: u32 = 0x0D;
    /// `VK_ESCAPE`.
    pub const ESCAPE: u32 = 0x1B;
    /// `VK_LEFT`, `VK_UP`, `VK_RIGHT`, `VK_DOWN`.
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    /// `VK_PRIOR` / `VK_NEXT` — Page Up and Page Down.
    pub const PRIOR: u32 = 0x21;
    pub const NEXT: u32 = 0x22;
    /// `VK_HOME` / `VK_END`.
    pub const HOME: u32 = 0x24;
    pub const END: u32 = 0x23;
    /// `VK_F5`, which the menu bar's accelerator table owns.
    pub const F5: u32 = 0x74;
}

/// The command a browse pane's `WM_KEYDOWN` should route, or `None` to leave the key to the
/// control. Enter → `BrowseOpenSelected` (the executor opens the selected grid image when the
/// grid is focused, else returns to image mode), Esc → `EnterImageMode`, Tab →
/// `ToggleBrowseFocus`.
#[must_use]
pub fn browse_keydown_command(virtual_key: u32) -> Option<AppCommand> {
    match virtual_key {
        vk::TAB => Some(AppCommand::ToggleBrowseFocus),
        vk::ESCAPE => Some(AppCommand::EnterImageMode),
        vk::RETURN => Some(AppCommand::BrowseOpenSelected),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_enter_and_escape_are_the_only_keys_taken() {
        assert!(matches!(
            browse_keydown_command(vk::TAB),
            Some(AppCommand::ToggleBrowseFocus)
        ));
        assert!(matches!(
            browse_keydown_command(vk::ESCAPE),
            Some(AppCommand::EnterImageMode)
        ));
        assert!(matches!(
            browse_keydown_command(vk::RETURN),
            Some(AppCommand::BrowseOpenSelected)
        ));
    }

    /// The keys that make the browser feel like Explorer are the ones we don't touch. Taking any
    /// of these would replace a native behaviour with a worse one: arrow selection, page scroll,
    /// jump to first and last, and type-select all come free from the controls.
    #[test]
    fn navigation_keys_fall_through_to_the_control() {
        for key in [
            vk::LEFT,
            vk::UP,
            vk::RIGHT,
            vk::DOWN,
            vk::PRIOR,
            vk::NEXT,
            vk::HOME,
            vk::END,
            vk::BACK,
            vk::F5,
        ] {
            assert!(
                browse_keydown_command(key).is_none(),
                "key {key:#04x} should reach the control"
            );
        }
        // Every letter, so type-select keeps working.
        for letter in 'A'..='Z' {
            assert!(browse_keydown_command(letter as u32).is_none());
        }
    }

    /// The macOS panes route the same three keys to the same three commands. A divergence here
    /// would mean Esc means one thing on one platform and another elsewhere. `AppCommand` isn't
    /// `Debug`, so the pairs are matched rather than compared.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_two_platforms_route_the_same_three_keys() {
        use crate::browser::browse_keydown_command as macos;
        // `kVK_Tab`, `kVK_Escape`, `kVK_Return`.
        assert!(matches!(
            (browse_keydown_command(vk::TAB), macos(48)),
            (
                Some(AppCommand::ToggleBrowseFocus),
                Some(AppCommand::ToggleBrowseFocus)
            )
        ));
        assert!(matches!(
            (browse_keydown_command(vk::ESCAPE), macos(53)),
            (
                Some(AppCommand::EnterImageMode),
                Some(AppCommand::EnterImageMode)
            )
        ));
        assert!(matches!(
            (browse_keydown_command(vk::RETURN), macos(36)),
            (
                Some(AppCommand::BrowseOpenSelected),
                Some(AppCommand::BrowseOpenSelected)
            )
        ));
    }
}
