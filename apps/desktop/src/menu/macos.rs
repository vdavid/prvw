//! How a menu item is dressed on macOS: the title it wears and the key equivalent AppKit fires.
//!
//! The shared half of a menu item is `parity::menu_items`: one [`MenuItemKey`] per item, one
//! `label` per key, the same string on every platform. This is the per-platform half, and
//! [`super::windows`] is its counterpart. Both compile on both platforms, so a Windows build
//! type-checks this table and a Mac runs the other one's tests (`menu/CLAUDE.md`).

use muda::accelerator::{Accelerator, Code, Modifiers};

use crate::parity::menu_items::MenuItemKey;

/// The exact string an item wears.
///
/// `label` is what the item is called right now, which is [`MenuItemKey::label`] for every item
/// except the two that flip with a mode. The registry's hint is padded onto the end, which is
/// what lines the shortcut column up under a proportional font.
pub fn title(key: MenuItemKey, label: &str) -> String {
    format!("{label}{}", key.hint())
}

/// A top-level menu's own title. macOS wants it plain.
pub fn menu_title(title: &'static str) -> String {
    title.to_string()
}

/// What File → Quit is called. `None` lets AppKit supply its own localized "Quit Prvw".
pub fn quit_text() -> Option<String> {
    None
}

/// Show or hide the menu bar, which macOS does for itself: entering fullscreen slides the
/// system bar away and leaving brings it back, so there's nothing for the app to do.
pub fn set_visible(_visible: bool) {}

/// The key equivalent AppKit fires for the item, or `None` where the key belongs to `input`.
///
/// `Modifiers::SUPER` is Command here. Bare-letter equivalents are deliberately absent: a menu
/// key equivalent is app-global, so `f`, `h`, or `e` would hijack those letters the moment a
/// settings text field has focus. Those keys live in `input::key_to_command`, and the item's
/// title advertises them instead.
///
/// Exhaustive with no `_` arm, like every other table built from the registry: a new menu item
/// can't be added without deciding whether it carries a shortcut here.
pub fn accelerator(key: MenuItemKey) -> Option<Accelerator> {
    let (modifiers, code) = match key {
        MenuItemKey::Settings => (Some(Modifiers::SUPER), Code::Comma),
        MenuItemKey::Open => (Some(Modifiers::SUPER), Code::KeyO),
        MenuItemKey::Print => (Some(Modifiers::SUPER), Code::KeyP),
        MenuItemKey::Copy => (Some(Modifiers::SUPER), Code::KeyC),
        MenuItemKey::ZoomIn => (Some(Modifiers::SUPER), Code::Equal),
        MenuItemKey::ZoomOut => (Some(Modifiers::SUPER), Code::Minus),
        MenuItemKey::ActualSize => (Some(Modifiers::SUPER), Code::Digit0),
        MenuItemKey::IccColorManagement => (Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyI),
        MenuItemKey::ColorMatchDisplay => (Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyC),
        MenuItemKey::RelativeColorimetric => {
            (Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyR)
        }
        MenuItemKey::GoToFirst => (None, Code::Home),
        MenuItemKey::GoToLast => (None, Code::End),
        MenuItemKey::SlideshowToggle => (Some(Modifiers::SUPER), Code::KeyS),

        // No shortcut, or one `input` owns. The toolkit's own items (Hide, Quit, Close window)
        // come with AppKit's standard equivalents already.
        MenuItemKey::About
        | MenuItemKey::Hide
        | MenuItemKey::HideOthers
        | MenuItemKey::ShowAll
        | MenuItemKey::Quit
        | MenuItemKey::CloseWindow
        | MenuItemKey::FitToWindow
        | MenuItemKey::AutoFitWindow
        | MenuItemKey::EnlargeSmallImages
        | MenuItemKey::Histogram
        | MenuItemKey::ExifInfo
        | MenuItemKey::SortByName
        | MenuItemKey::SortByDate
        | MenuItemKey::SortByFileType
        | MenuItemKey::Fullscreen
        | MenuItemKey::Refresh
        | MenuItemKey::BrowseToggle
        | MenuItemKey::Previous
        | MenuItemKey::Next
        | MenuItemKey::LoopNavigation
        | MenuItemKey::SlideshowIncreaseSpeed
        | MenuItemKey::SlideshowDecreaseSpeed
        | MenuItemKey::ContextCopy
        | MenuItemKey::ContextPrint => return None,
    };
    Some(Accelerator::new(modifiers, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two items firing on the same keystroke means one of them never runs, and AppKit picks
    /// the winner by menu order rather than by anything we decided.
    #[test]
    fn no_two_items_share_a_shortcut() {
        let mut taken: Vec<(Accelerator, MenuItemKey)> = Vec::new();
        for key in MenuItemKey::ALL {
            let Some(accelerator) = accelerator(*key) else {
                continue;
            };
            if let Some((_, other)) = taken.iter().find(|(a, _)| *a == accelerator) {
                panic!("{} and {} share a shortcut", key.name(), other.name());
            }
            taken.push((accelerator, *key));
        }
    }

    /// The padded hint the registry declares is what lines the shortcut column up.
    #[test]
    fn a_title_carries_the_registrys_padded_hint() {
        let browse = MenuItemKey::BrowseToggle;
        assert_eq!(
            title(browse, browse.label()),
            "Image browser        \u{23ce}"
        );
        assert_eq!(title(browse, browse.label()), browse.title());
    }
}
