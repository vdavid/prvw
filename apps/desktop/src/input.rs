//! Maps keyboard, mouse, and menu events to `AppCommand`s.
//!
//! This is the single place that defines what each input does. The main event loop,
//! menu handler, and QA key handler all call into these functions rather than
//! duplicating action logic.

use crate::commands::AppCommand;
use crate::menu::MenuIds;
use muda::MenuEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Map a keyboard key press to an `AppCommand`.
/// Returns `None` for keys that don't map to any action.
/// Takes `Key<&str>` (from `Key::as_ref()`) so callers don't need to clone.
pub fn key_to_command(key: Key<&str>, modifiers: &ModifiersState) -> Option<AppCommand> {
    // Bare H toggles the histogram, bare E toggles the EXIF info panel, and
    // bare L toggles loop navigation. We deliberately ignore the press if
    // any modifier is held so Cmd-H (Hide window) and friends keep their
    // native behavior.
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
    match key {
        // Navigation (user input → debounced so a wheel spin coalesces)
        Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::Backspace) | Key::Character("[") => {
            Some(AppCommand::NavigateDebounced(false))
        }
        Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::Space) | Key::Character("]") => {
            Some(AppCommand::NavigateDebounced(true))
        }
        Key::Named(NamedKey::Home) => Some(AppCommand::GoToFirst),
        Key::Named(NamedKey::End) => Some(AppCommand::GoToLast),

        // Fullscreen
        Key::Named(NamedKey::F11) | Key::Named(NamedKey::Enter) | Key::Character("f") => {
            Some(AppCommand::ToggleFullscreen)
        }

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

/// Map a menu event to an `AppCommand`, using the menu's ID table.
pub fn menu_to_command(event: &MenuEvent, ids: &MenuIds) -> Option<AppCommand> {
    let id = event.id();
    if id == &ids.about {
        Some(AppCommand::ShowAbout)
    } else if id == &ids.settings {
        Some(AppCommand::ShowSettings)
    } else if id == &ids.copy || id == &ids.context_copy {
        Some(AppCommand::CopyImage)
    } else if id == &ids.print || id == &ids.context_print {
        Some(AppCommand::Print)
    } else if id == &ids.zoom_in {
        Some(AppCommand::ZoomIn)
    } else if id == &ids.zoom_out {
        Some(AppCommand::ZoomOut)
    } else if id == &ids.actual_size {
        Some(AppCommand::ActualSize)
    } else if id == &ids.fit_to_window {
        Some(AppCommand::FitToWindow)
    } else if id == &ids.auto_fit_window
        || id == &ids.enlarge_small_images
        || id == &ids.icc_color_management
        || id == &ids.color_match_display
        || id == &ids.histogram
        || id == &ids.exif_info
        || id == &ids.loop_navigation
    {
        // CheckMenuItems auto-toggle on click; we return None and let the caller
        // handle it (it needs the CheckMenuItem ref to read the new state).
        None
    } else if id == &ids.sort_by_name {
        Some(AppCommand::SetSortBy(crate::navigation::SortBy::Name))
    } else if id == &ids.sort_by_date {
        Some(AppCommand::SetSortBy(crate::navigation::SortBy::Date))
    } else if id == &ids.sort_by_file_type {
        Some(AppCommand::SetSortBy(crate::navigation::SortBy::FileType))
    } else if id == &ids.fullscreen {
        Some(AppCommand::ToggleFullscreen)
    } else if id == &ids.refresh {
        Some(AppCommand::Refresh)
    } else if id == &ids.previous {
        Some(AppCommand::NavigateDebounced(false))
    } else if id == &ids.next {
        Some(AppCommand::NavigateDebounced(true))
    } else if id == &ids.go_to_first {
        Some(AppCommand::GoToFirst)
    } else if id == &ids.go_to_last {
        Some(AppCommand::GoToLast)
    } else {
        None
    }
}

/// Map a QA server key name (web conventions) to an `AppCommand`.
pub fn qa_key_to_command(key_name: &str) -> Option<AppCommand> {
    match key_name {
        "ArrowLeft" | "Backspace" | "[" => Some(AppCommand::Navigate(false)),
        "ArrowRight" | " " | "Space" | "]" => Some(AppCommand::Navigate(true)),
        "Home" => Some(AppCommand::GoToFirst),
        "End" => Some(AppCommand::GoToLast),
        "Enter" | "F11" | "f" => Some(AppCommand::ToggleFullscreen),
        "Escape" => Some(AppCommand::Exit),
        "+" | "=" => Some(AppCommand::ZoomIn),
        "-" => Some(AppCommand::ZoomOut),
        "0" => Some(AppCommand::FitToWindow),
        "1" => Some(AppCommand::ActualSize),
        "h" | "H" => Some(AppCommand::ToggleHistogram),
        "e" | "E" => Some(AppCommand::ToggleExifInfo),
        "l" | "L" => Some(AppCommand::ToggleLoopNavigation),
        "r" => Some(AppCommand::Refresh),
        _ => {
            log::debug!("QA server: unhandled key '{key_name}'");
            None
        }
    }
}
