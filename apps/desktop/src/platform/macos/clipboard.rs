//! macOS clipboard: copy the current image to the general pasteboard.
//!
//! Writes two representations so each paste target gets what it wants:
//! - the file URL (`NSURL`) → Finder, Mail, etc. paste the original file (full quality + EXIF)
//! - a bitmap (`NSImage`) → editors and chat apps paste pixels
//!
//! We copy from the original file on disk, not the in-memory decode: that buffer is already
//! transformed to the display profile (and may be HDR half-float), so handing it to another
//! app would shift colors.

use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSString, NSURL};

use super::ui_common::load_image_from_path;

/// Copy the image at `path` to the general pasteboard as a file URL plus a bitmap.
/// Returns `false` if the image data couldn't be loaded (missing/unreadable file) or the
/// pasteboard write failed.
pub(crate) fn copy_image_file(path: &Path) -> bool {
    let pasteboard = NSPasteboard::generalPasteboard();
    write_image_to_pasteboard(&pasteboard, path)
}

/// Write the file-URL + bitmap representations of `path` to `pasteboard`. Split from
/// `copy_image_file` so tests can target a scratch pasteboard instead of the user's
/// real clipboard.
fn write_image_to_pasteboard(pasteboard: &NSPasteboard, path: &Path) -> bool {
    // `NSImage` loaded from the file provides the TIFF representation for bitmap consumers,
    // color-managed by its embedded ICC profile (see `load_image_from_path`).
    let Some(image) = load_image_from_path(path) else {
        return false;
    };
    let path_str = path.to_string_lossy();
    // SAFETY: standard AppKit pasteboard calls. Invoked from the command executor, which
    // runs on the winit main thread. Every object stays alive for the `writeObjects` call.
    unsafe {
        let ns_path = NSString::from_str(&path_str);
        let url: Retained<NSURL> = msg_send![class!(NSURL), fileURLWithPath: &*ns_path];

        // Order matters: file-URL first so Finder and friends prefer pasting the file;
        // bitmap second for apps that only take pixels.
        let objects: Retained<NSArray<AnyObject>> = NSArray::from_retained_slice(&[
            Retained::cast_unchecked::<AnyObject>(url),
            Retained::cast_unchecked::<AnyObject>(image),
        ]);

        pasteboard.clearContents();
        let ok: bool = msg_send![pasteboard, writeObjects: &*objects];
        if !ok {
            log::warn!("Copy image: pasteboard writeObjects failed for {path_str}");
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Writing a real image must leave both a file-URL and a bitmap (TIFF) on the
    /// pasteboard, so Finder gets the file and editors get pixels. Uses a unique scratch
    /// pasteboard so the test doesn't clobber the developer's real clipboard.
    #[test]
    fn copy_writes_file_url_and_bitmap() {
        let path = fixture("p3_red_64x64.jpg");
        assert!(path.exists(), "fixture missing: {}", path.display());

        unsafe {
            let pasteboard = NSPasteboard::pasteboardWithUniqueName();
            assert!(write_image_to_pasteboard(&pasteboard, &path));

            let types: Retained<NSArray<NSString>> = msg_send![&*pasteboard, types];
            let mut type_strings = Vec::new();
            for i in 0..types.count() {
                let t: Retained<NSString> = msg_send![&*types, objectAtIndex: i];
                type_strings.push(t.to_string());
            }

            assert!(
                type_strings.iter().any(|t| t == "public.file-url"),
                "expected a file-URL type, got {type_strings:?}"
            );
            assert!(
                type_strings.iter().any(|t| t == "public.tiff"),
                "expected a TIFF bitmap type, got {type_strings:?}"
            );

            let _: () = msg_send![&*pasteboard, releaseGlobally];
        }
    }

    /// A missing file can't be loaded as an image, so copy reports failure rather than
    /// silently putting a broken URL on the clipboard.
    #[test]
    fn copy_missing_file_returns_false() {
        let path = fixture("does-not-exist.jpg");
        unsafe {
            let pasteboard = NSPasteboard::pasteboardWithUniqueName();
            assert!(!write_image_to_pasteboard(&pasteboard, &path));
            let _: () = msg_send![&*pasteboard, releaseGlobally];
        }
    }
}
