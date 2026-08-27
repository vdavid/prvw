//! # About Prvw
//!
//! One box, forked per platform. What it says is [`content`], shared and tested on every host;
//! how it looks is the platform module beside it, because an About box is the smallest window a
//! product has and the one where looking foreign is most obvious.
//!
//! - macOS: a non-modal `NSWindow`, opened from the Prvw menu or Cmd+Shift+A.
//! - Windows: a modeless Win32 popup under Help, which is where Windows keeps About.
//! - Linux: nothing yet. There's no menu bar to open it from (`menu/absent.rs`).

pub mod content;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;
