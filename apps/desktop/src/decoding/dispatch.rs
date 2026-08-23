//! Extension-based backend selection.
//!
//! `pick_backend` maps a file extension to the decoder that should handle it. The three
//! extension tables are the single source of truth: the predicates, `pick_backend`, and the
//! file picker's filter (`supported_extensions`) all read them.

/// The decoder that will handle a given file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Backend {
    /// Fast SIMD JPEG path via `zune-jpeg`.
    Jpeg,
    /// Camera RAW via `rawler`.
    Raw,
    /// Fallback via the `image` crate (PNG, GIF, WebP, BMP, TIFF).
    Generic,
}

/// Pick the decoder for a file extension. Unknown extensions fall through to
/// `Generic`; callers gate on [`is_supported_extension`] first.
pub(super) fn pick_backend(ext: &str) -> Backend {
    if is_jpeg_extension(ext) {
        Backend::Jpeg
    } else if is_raw_extension(ext) {
        Backend::Raw
    } else {
        Backend::Generic
    }
}

/// JPEG extensions eligible for the fast zune-jpeg decode path.
const JPEG_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe", "jfif"];

/// Camera RAW extensions handled by the `rawler` backend.
const RAW_EXTENSIONS: &[&str] = &[
    "dng", "cr2", "cr3", "nef", "arw", "orf", "raf", "rw2", "pef", "srw",
];

/// Extensions the generic `image` crate backend handles.
const GENERIC_EXTENSIONS: &[&str] = &["png", "gif", "webp", "bmp", "tiff", "tif"];

pub(super) fn is_jpeg_extension(ext: &str) -> bool {
    contains(JPEG_EXTENSIONS, ext)
}

pub(super) fn is_raw_extension(ext: &str) -> bool {
    contains(RAW_EXTENSIONS, ext)
}

pub(super) fn is_generic_extension(ext: &str) -> bool {
    contains(GENERIC_EXTENSIONS, ext)
}

/// Whether any backend claims this extension.
pub(super) fn is_supported_extension(ext: &str) -> bool {
    is_jpeg_extension(ext) || is_raw_extension(ext) || is_generic_extension(ext)
}

/// Every extension the app opens, for the file picker's filter. Built from the same three
/// tables the predicates read, so a format can't be openable and yet invisible in the picker.
pub(super) fn supported_extensions() -> Vec<&'static str> {
    JPEG_EXTENSIONS
        .iter()
        .chain(RAW_EXTENSIONS)
        .chain(GENERIC_EXTENSIONS)
        .copied()
        .collect()
}

/// Extensions arrive from `Path::extension`, so they carry whatever case the file has.
fn contains(table: &[&str], ext: &str) -> bool {
    table.iter().any(|known| known.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_extensions_match_case_insensitively() {
        for ext in [
            "dng", "cr2", "cr3", "nef", "arw", "orf", "raf", "rw2", "pef", "srw",
        ] {
            assert!(is_raw_extension(ext), "{ext} should be RAW");
            assert!(
                is_raw_extension(&ext.to_ascii_uppercase()),
                "{} should be RAW",
                ext.to_ascii_uppercase()
            );
        }
    }

    #[test]
    fn non_raw_extensions_are_rejected() {
        for ext in [
            "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "tif", "",
        ] {
            assert!(!is_raw_extension(ext), "{ext} should not be RAW");
        }
    }

    #[test]
    fn pick_backend_routes_correctly() {
        assert_eq!(pick_backend("jpg"), Backend::Jpeg);
        assert_eq!(pick_backend("JPEG"), Backend::Jpeg);
        assert_eq!(pick_backend("dng"), Backend::Raw);
        assert_eq!(pick_backend("ARW"), Backend::Raw);
        assert_eq!(pick_backend("cr3"), Backend::Raw);
        assert_eq!(pick_backend("png"), Backend::Generic);
        assert_eq!(pick_backend("tif"), Backend::Generic);
        // Unknown extensions fall through to Generic; the supported-extension
        // gate is what filters them out upstream.
        assert_eq!(pick_backend("xyz"), Backend::Generic);
    }

    #[test]
    fn the_picker_filter_lists_exactly_what_we_open() {
        for ext in supported_extensions() {
            assert!(
                is_supported_extension(ext),
                "{ext} is offered but not opened"
            );
        }
        assert!(!supported_extensions().contains(&"txt"));
    }

    #[test]
    fn is_supported_extension_covers_all_raw_formats() {
        for ext in [
            "dng", "cr2", "cr3", "nef", "arw", "orf", "raf", "rw2", "pef", "srw",
        ] {
            assert!(is_supported_extension(ext), "{ext} should be supported");
        }
        assert!(is_supported_extension("jpg"));
        assert!(is_supported_extension("png"));
        assert!(!is_supported_extension("txt"));
        assert!(!is_supported_extension("mov"));
    }
}
