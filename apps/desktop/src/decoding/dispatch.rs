//! Extension-based backend selection.
//!
//! `pick_backend` maps a file extension to the decoder that should handle it.
//! Keep this table in sync with `is_supported_extension` — they're the two sides
//! of the same coin.

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
pub(super) fn is_jpeg_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "jpe" | "jfif"
    )
}

/// Camera RAW extensions handled by the `rawler` backend.
pub(super) fn is_raw_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "raf" | "rw2" | "pef" | "srw"
    )
}

/// Extensions the generic `image` crate backend handles.
pub(super) fn is_generic_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "gif" | "webp" | "bmp" | "tiff" | "tif"
    )
}

/// Whether any backend claims this extension.
pub(super) fn is_supported_extension(ext: &str) -> bool {
    is_jpeg_extension(ext) || is_raw_extension(ext) || is_generic_extension(ext)
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
