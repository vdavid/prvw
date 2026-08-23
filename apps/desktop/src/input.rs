//! Maps keyboard and QA-server key events to `AppCommand`s.
//!
//! This is the single place that defines what each key does, in image mode and in browse mode,
//! for real keystrokes and for the QA server's synthetic ones alike. The menu's twin table
//! lives in `menu::native`, next to the item IDs it has to match against.

use crate::commands::AppCommand;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Whether the modifier a platform reserves for its own app shortcuts is down: Cmd on macOS,
/// Ctrl everywhere else. Windows and Linux hand the Super key to the desktop, so `super_key()`
/// is never ours there.
fn command_modifier(modifiers: &ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    }
}

/// Map a keyboard key press to an `AppCommand`.
/// Returns `None` for keys that don't map to any action.
/// Takes `Key<&str>` (from `Key::as_ref()`) so callers don't need to clone.
pub fn key_to_command(key: Key<&str>, modifiers: &ModifiersState) -> Option<AppCommand> {
    // Bare H toggles the histogram, bare E toggles the EXIF info panel, bare L
    // toggles loop navigation, and bare S starts or stops the slideshow. We
    // deliberately ignore the press if any modifier is held so Cmd-H (Hide
    // window), Cmd-S (the Slideshow menu's own accelerator on macOS), and
    // friends keep their native behavior.
    let bare = !modifiers.shift_key()
        && !modifiers.control_key()
        && !modifiers.alt_key()
        && !modifiers.super_key();
    if bare && matches!(key, Key::Character("h") | Key::Character("H")) {
        return Some(AppCommand::ToggleHistogram);
    }
    if bare && matches!(key, Key::Character("e") | Key::Character("E")) {
        return Some(AppCommand::ToggleExifInfo);
    }
    if bare && matches!(key, Key::Character("l") | Key::Character("L")) {
        return Some(AppCommand::ToggleLoopNavigation);
    }
    if bare && matches!(key, Key::Character("s") | Key::Character("S")) {
        return Some(AppCommand::ToggleSlideshow);
    }
    // Open a file. The menu bar advertises the same shortcut, and on macOS AppKit's key
    // equivalent takes the press before winit ever sees it — so this arm is what serves
    // Windows and Linux, where nothing attaches the bar (`menu::absent`, and M4 for Windows).
    if command_modifier(modifiers) && matches!(key, Key::Character("o") | Key::Character("O")) {
        return Some(AppCommand::ShowOpenDialog);
    }
    match key {
        // Navigation (user input → debounced so a wheel spin coalesces).
        // `;` / `'` sit under the right hand next to the speed keys `[` / `]`.
        Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::Backspace) | Key::Character(";") => {
            Some(AppCommand::NavigateDebounced(false))
        }
        Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::Space) | Key::Character("'") => {
            Some(AppCommand::NavigateDebounced(true))
        }
        Key::Named(NamedKey::Home) => Some(AppCommand::GoToFirst),
        Key::Named(NamedKey::End) => Some(AppCommand::GoToLast),

        // Slideshow speed: `]` faster (fewer seconds), `[` slower. Always
        // adjusts the time-per-image setting, whether or not a slideshow is
        // running.
        Key::Character("]") => Some(AppCommand::IncreaseSlideshowSpeed),
        Key::Character("[") => Some(AppCommand::DecreaseSlideshowSpeed),

        // Fullscreen. `f` and `F11` keep toggling fullscreen. Enter used to as well,
        // but now enters the image browser instead (the capability isn't lost — only
        // Enter's binding moved). In image mode no native view is first responder, so
        // winit delivers Enter here; once in browse mode, the focused pane's `keyDown:`
        // override owns Enter (see `browser::browse_keydown_command`).
        Key::Named(NamedKey::F11) | Key::Character("f") => Some(AppCommand::ToggleFullscreen),
        Key::Named(NamedKey::Enter) => Some(AppCommand::ToggleBrowseMode),

        // Escape: exit fullscreen or exit app (handled specially in main.rs)
        Key::Named(NamedKey::Escape) => Some(AppCommand::Exit),

        // Zoom
        Key::Character("=" | "+") => Some(AppCommand::ZoomIn),
        Key::Character("-") => Some(AppCommand::ZoomOut),
        Key::Character("0") => Some(AppCommand::FitToWindow),
        Key::Character("1") => Some(AppCommand::ActualSize),

        _ => None,
    }
}

/// Map a keyboard key press to an `AppCommand` **while browse mode is active**.
///
/// In idle-winit browse mode the focused native pane holds first responder and handles its own
/// keys: arrows/page-keys/type-select stay native, and Tab/Enter/Esc are intercepted by the pane's
/// `keyDown:` override (see `browser::browse_keydown_command`). So winit normally delivers nothing
/// in browse mode. This mapping stays as a defensive fallback for Tab/Enter/Esc in case winit ever
/// does deliver a key here; arrows are deliberately NOT mapped (they're native). Returns `None` for
/// everything else.
pub fn browse_key_to_command(key: Key<&str>, _modifiers: &ModifiersState) -> Option<AppCommand> {
    match key {
        // Esc leaves browse mode for image mode (showing the current image).
        Key::Named(NamedKey::Escape) => Some(AppCommand::EnterImageMode),
        // Enter opens the selected image when the grid is focused; on the tree it returns to image
        // mode. The executor branches on the focused pane (`BrowseOpenSelected` falls back to
        // `EnterImageMode` when the grid isn't focused or has no selection).
        Key::Named(NamedKey::Enter) => Some(AppCommand::BrowseOpenSelected),
        // Tab flips focus between the tree and grid panes.
        Key::Named(NamedKey::Tab) => Some(AppCommand::ToggleBrowseFocus),
        _ => None,
    }
}

/// Map a QA server key name (web conventions) to an `AppCommand`.
pub fn qa_key_to_command(key_name: &str) -> Option<AppCommand> {
    match key_name {
        "ArrowLeft" | "Backspace" | ";" => Some(AppCommand::Navigate(false)),
        "ArrowRight" | " " | "Space" | "'" => Some(AppCommand::Navigate(true)),
        "]" => Some(AppCommand::IncreaseSlideshowSpeed),
        "[" => Some(AppCommand::DecreaseSlideshowSpeed),
        "Home" => Some(AppCommand::GoToFirst),
        "End" => Some(AppCommand::GoToLast),
        "F11" | "f" => Some(AppCommand::ToggleFullscreen),
        "Enter" => Some(AppCommand::ToggleBrowseMode),
        "Escape" => Some(AppCommand::Exit),
        "+" | "=" => Some(AppCommand::ZoomIn),
        "-" => Some(AppCommand::ZoomOut),
        "0" => Some(AppCommand::FitToWindow),
        "1" => Some(AppCommand::ActualSize),
        "h" | "H" => Some(AppCommand::ToggleHistogram),
        "e" | "E" => Some(AppCommand::ToggleExifInfo),
        "l" | "L" => Some(AppCommand::ToggleLoopNavigation),
        "s" | "S" => Some(AppCommand::ToggleSlideshow),
        "r" => Some(AppCommand::Refresh),
        _ => {
            log::debug!("QA server: unhandled key '{key_name}'");
            None
        }
    }
}

/// Map a QA server key name to an `AppCommand` **while browse mode is active** — the QA-path
/// twin of `browse_key_to_command`. The `SendKey` handler picks this when `browser.is_browse()`
/// so tests drive the same browse routing as real keystrokes.
pub fn browse_qa_key_to_command(key_name: &str) -> Option<AppCommand> {
    match key_name {
        "Escape" => Some(AppCommand::EnterImageMode),
        "Enter" => Some(AppCommand::BrowseOpenSelected),
        "Tab" => Some(AppCommand::ToggleBrowseFocus),
        // Arrows are native (the focused pane's first responder handles them); the QA path can't
        // drive native selection by key, so it doesn't map them. Tab/Enter/Esc cover the testable
        // focus/mode transitions.
        _ => {
            log::debug!("QA server (browse mode): unhandled key '{key_name}'");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> ModifiersState {
        ModifiersState::empty()
    }

    /// Bare `s` starts or stops the slideshow, in either case.
    #[test]
    fn bare_s_toggles_the_slideshow() {
        for key in [Key::Character("s"), Key::Character("S")] {
            assert!(matches!(
                key_to_command(key, &bare()),
                Some(AppCommand::ToggleSlideshow)
            ));
        }
    }

    /// A held modifier hands the press back to the platform, so macOS's own Cmd-S menu
    /// accelerator and the Windows Save muscle memory both keep their meaning.
    #[test]
    fn modified_s_is_left_alone() {
        for modifiers in [
            ModifiersState::SUPER,
            ModifiersState::CONTROL,
            ModifiersState::ALT,
            ModifiersState::SHIFT,
        ] {
            assert!(key_to_command(Key::Character("s"), &modifiers).is_none());
        }
    }

    /// Browse mode's tree and list own bare letters for type-ahead select, so `s` belongs to
    /// the focused pane there rather than to the slideshow.
    #[test]
    fn browse_mode_leaves_s_to_the_focused_pane() {
        assert!(browse_key_to_command(Key::Character("s"), &bare()).is_none());
        assert!(browse_qa_key_to_command("s").is_none());
    }

    /// The QA table is the real keyboard's twin, so a binding that lands in one lands in both.
    #[test]
    fn the_qa_table_drives_the_slideshow_too() {
        for name in ["s", "S"] {
            assert!(matches!(
                qa_key_to_command(name),
                Some(AppCommand::ToggleSlideshow)
            ));
        }
    }
}
