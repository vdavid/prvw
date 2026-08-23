//! What Prvw opens when it starts, decided from the command line.
//!
//! Two questions, both answered here so they're testable for every platform from any host, the
//! way `parity` is. `main` asks the first before the event loop exists; `App::initialize_viewer`
//! asks the second while it's building the navigation list.

use std::path::{Path, PathBuf};

use crate::decoding;
use crate::parity::Platform;

/// Whether to hold the window back and wait for a file to arrive, instead of opening one.
///
/// True only on macOS with nothing named on the command line. There, Finder delivers a
/// double-clicked file through an Apple Event rather than argv
/// (`platform/macos/open_handler.rs`) and `onboarding` puts a window up meanwhile, so waiting is
/// the normal first-run path. Nothing is coming on Windows or Linux, where a Start-menu shortcut,
/// a taskbar pin, and a desktop icon all launch with no argv at all — so the window opens on the
/// empty state (`app::EmptyState::NothingOpen`) and the user picks a file from there.
pub fn waits_for_a_file(nothing_named: bool, platform: Platform) -> bool {
    nothing_named && platform == Platform::MacOs
}

/// The images in `dir`, in whatever order the filesystem hands them over.
///
/// Callers sort: `DirectoryList::from_explicit` puts them in the user's chosen order. Subfolders
/// are not walked, matching what opening any image in a folder gives you.
pub fn images_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        log::warn!("Couldn't read the folder {}", dir.display());
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(decoding::is_supported_extension)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_macos_waits_for_a_file_to_arrive() {
        assert!(waits_for_a_file(true, Platform::MacOs));
        assert!(!waits_for_a_file(true, Platform::Windows));
        assert!(!waits_for_a_file(true, Platform::Linux));
    }

    #[test]
    fn naming_something_never_waits() {
        for platform in Platform::ALL {
            assert!(!waits_for_a_file(false, *platform));
        }
    }

    #[test]
    fn a_folder_offers_its_images_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.png", "a.jpg", "notes.txt", "raw.cr2"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        std::fs::create_dir(dir.path().join("subfolder")).unwrap();

        let mut found: Vec<String> = images_in(dir.path())
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, vec!["a.jpg", "b.png", "raw.cr2"]);
    }

    #[test]
    fn a_folder_with_no_images_offers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        assert!(images_in(dir.path()).is_empty());
    }

    #[test]
    fn an_unreadable_folder_offers_nothing_rather_than_panicking() {
        assert!(images_in(Path::new("/no/such/folder/anywhere")).is_empty());
    }
}
