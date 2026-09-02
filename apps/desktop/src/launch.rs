//! What Prvw is being asked to open, whether that came from the command line or a drop.
//!
//! Every question here is answered purely, so it's testable for every platform from any host,
//! the way `parity` is. `main` asks [`waits_for_a_file`] before the event loop exists, and
//! `App::open_dropped` asks [`classify_open_request`] each time files land on the window. Reading
//! the folder itself is `crate::folder_scan`'s job, on its own thread.

use std::path::PathBuf;

use crate::browser::LaunchTarget;
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

/// What a set of paths handed to Prvw at once resolves to. See [`classify_open_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenRequest {
    /// Nothing Prvw can open: no readable path, or nothing among them it decodes.
    Nothing,
    /// Show these images, starting at the first. One is the ordinary case.
    Images(Vec<PathBuf>),
    /// One folder, which each platform answers differently (browse mode on macOS and Windows,
    /// the folder's images in image mode on Linux), exactly as a folder on the command line does.
    Folder(PathBuf),
}

/// Decide what a drop opens, from each path and what it is on disk.
///
/// `targets` pairs a path with [`crate::browser::LaunchTarget`], the same classification a
/// command-line argument gets, so the two routes can't drift on the question that matters: a
/// lone folder means "show me this folder", and anything else is a set of images.
///
/// Two things differ from the command line, both because a drop is a bulk gesture rather than a
/// typed path:
///
/// - **Unsupported files are dropped, not opened.** Someone dragging a folder's worth of mixed
///   files means the pictures among them. A typed `prvw notes.txt` is trusted and shows its
///   decode error; a dropped one would take the image on screen away, so it's ignored instead.
///   The extension gate is [`crate::decoding::is_supported_extension`], the same one that picks
///   a folder's images.
/// - **Folders alongside anything else are ignored**, since a set of images is the only thing
///   the app can show at once. The command line takes the same line: no browsing two folders.
///
/// Pure, so every platform's answer is checked from any host.
#[must_use]
pub fn classify_open_request(targets: &[(PathBuf, LaunchTarget)]) -> OpenRequest {
    if let [(folder, LaunchTarget::Directory)] = targets {
        return OpenRequest::Folder(folder.clone());
    }
    let images: Vec<PathBuf> = targets
        .iter()
        .filter(|(_, kind)| *kind == LaunchTarget::Image)
        .map(|(path, _)| path.clone())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(decoding::is_supported_extension)
        })
        .collect();
    if images.is_empty() {
        OpenRequest::Nothing
    } else {
        OpenRequest::Images(images)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(name: &str) -> (PathBuf, LaunchTarget) {
        (PathBuf::from(name), LaunchTarget::Image)
    }

    fn folder(name: &str) -> (PathBuf, LaunchTarget) {
        (PathBuf::from(name), LaunchTarget::Directory)
    }

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

    /// The ordinary drop: one picture, which opens.
    #[test]
    fn one_image_opens_that_image() {
        assert_eq!(
            classify_open_request(&[image("a.jpg")]),
            OpenRequest::Images(vec![PathBuf::from("a.jpg")])
        );
    }

    /// A multi-image drop keeps the order it arrived in, because the first one is the one that
    /// opens and the arrows walk the rest.
    #[test]
    fn several_images_become_one_set_in_drop_order() {
        assert_eq!(
            classify_open_request(&[image("c.jpg"), image("a.png"), image("b.cr2")]),
            OpenRequest::Images(vec![
                PathBuf::from("c.jpg"),
                PathBuf::from("a.png"),
                PathBuf::from("b.cr2"),
            ])
        );
    }

    /// A folder on its own is the case each platform answers differently, and it has to reach
    /// the caller as a folder rather than being flattened here.
    #[test]
    fn one_folder_stays_a_folder() {
        assert_eq!(
            classify_open_request(&[folder("/photos")]),
            OpenRequest::Folder(PathBuf::from("/photos"))
        );
    }

    /// Dropping something Prvw can't decode leaves the viewer alone. Opening it would replace
    /// the image on screen with a title-bar error, which is worse than nothing happening.
    #[test]
    fn files_prvw_cant_open_are_ignored() {
        assert_eq!(
            classify_open_request(&[image("notes.txt"), image("no-extension")]),
            OpenRequest::Nothing
        );
        assert_eq!(
            classify_open_request(&[image("notes.txt"), image("a.jpg")]),
            OpenRequest::Images(vec![PathBuf::from("a.jpg")])
        );
    }

    /// Prvw shows one set of images at a time, so a folder in a bigger drop is dropped rather
    /// than expanded. Same call the command line makes for multiple arguments.
    #[test]
    fn a_folder_alongside_images_is_ignored() {
        assert_eq!(
            classify_open_request(&[folder("/photos"), image("a.jpg")]),
            OpenRequest::Images(vec![PathBuf::from("a.jpg")])
        );
        assert_eq!(
            classify_open_request(&[folder("/photos"), folder("/more")]),
            OpenRequest::Nothing
        );
    }

    /// A path that's neither (deleted between the drag starting and the drop, or unreadable)
    /// can't take the rest of the drop down with it.
    #[test]
    fn a_path_that_is_neither_file_nor_folder_is_skipped() {
        assert_eq!(
            classify_open_request(&[
                (PathBuf::from("gone.jpg"), LaunchTarget::Onboarding),
                image("a.jpg"),
            ]),
            OpenRequest::Images(vec![PathBuf::from("a.jpg")])
        );
        assert_eq!(classify_open_request(&[]), OpenRequest::Nothing);
    }
}
