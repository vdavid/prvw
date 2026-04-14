//! Maps keyboard, mouse, and menu events to `AppCommand`s.
//!
//! This is the single place that defines what each input does. The main event loop,
//! menu handler, and QA key handler all call into these functions rather than
//! duplicating action logic.

use crate::menu::MenuIds;
use crate::qa_server::AppCommand;
use muda::MenuEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Map a keyboard key press to an `AppCommand`.
/// Returns `None` for keys that don't map to any action.
/// Takes `Key<&str>` (from `Key::as_ref()`) so callers don't need to clone.
pub fn key_to_command(key: Key<&str>, _modifiers: &ModifiersState) -> Option<AppCommand> {
    match key {
        // Navigation
        Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::Backspace) | Key::Character("[") => {
            Some(AppCommand::Navigate(false))
        }
        Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::Space) | Key::Character("]") => {
            Some(AppCommand::Navigate(true))
        }

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
    } else if id == &ids.zoom_in {
        Some(AppCommand::ZoomIn)
    } else if id == &ids.zoom_out {
        Some(AppCommand::ZoomOut)
    } else if id == &ids.actual_size {
        Some(AppCommand::ActualSize)
    } else if id == &ids.fit_to_window {
        Some(AppCommand::FitToWindow)
    } else if id == &ids.auto_fit_window {
        // CheckMenuItem auto-toggles on click; we don't know the new state here,
        // so we return None and let the caller handle it (it needs the CheckMenuItem ref).
        None
    } else if id == &ids.fullscreen {
        Some(AppCommand::ToggleFullscreen)
    } else if id == &ids.previous {
        Some(AppCommand::Navigate(false))
    } else if id == &ids.next {
        Some(AppCommand::Navigate(true))
    } else {
        None
    }
}

/// Map a QA server key name (web conventions) to an `AppCommand`.
pub fn qa_key_to_command(key_name: &str) -> Option<AppCommand> {
    match key_name {
        "ArrowLeft" | "Backspace" | "[" => Some(AppCommand::Navigate(false)),
        "ArrowRight" | " " | "Space" | "]" => Some(AppCommand::Navigate(true)),
        "Enter" | "F11" | "f" => Some(AppCommand::ToggleFullscreen),
        "Escape" => Some(AppCommand::Exit),
        "+" | "=" => Some(AppCommand::ZoomIn),
        "-" => Some(AppCommand::ZoomOut),
        "0" => Some(AppCommand::FitToWindow),
        "1" => Some(AppCommand::ActualSize),
        _ => {
            log::debug!("QA server: unhandled key '{key_name}'");
            None
        }
    }
}
