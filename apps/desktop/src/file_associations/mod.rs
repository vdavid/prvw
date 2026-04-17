//! File association helpers: queries, registration, and restore.
//!
//! Used by both the onboarding window and the Settings file associations UI.
//! Uses CoreServices FFI directly (via objc2-core-services) — near-instant,
//! no Swift toolchain dependency.

pub mod settings_panel;

// These CoreServices functions are deprecated in favor of NSWorkspace async methods,
// but the sync versions are simpler and work fine for our use case.
use objc2_core_foundation::CFString;
#[allow(deprecated)]
use objc2_core_services::{
    LSCopyDefaultRoleHandlerForContentType, LSRolesMask, LSSetDefaultRoleHandlerForContentType,
};

pub const BUNDLE_ID: &str = "com.veszelovszki.prvw";
pub const DEFAULT_FALLBACK: &str = "com.apple.Preview";

pub struct UtiEntry {
    pub uti: &'static str,
    /// Short format name shown in the Settings row (e.g., "JPEG", "CR3").
    pub label: &'static str,
    /// User-facing hint: extension list for standard formats, vendor name for RAW.
    pub detail: &'static str,
}

/// Number of standard image formats in [`SUPPORTED_UTIS`]. The remaining entries are RAW.
const STANDARD_COUNT: usize = 6;

/// Every UTI Prvw handles. Standard formats come first, then RAW formats. The Settings
/// panel slices this via [`SUPPORTED_STANDARD_UTIS`] and [`SUPPORTED_RAW_UTIS`].
///
/// Keep this in sync with `CFBundleDocumentTypes` in `Info.plist` and with the decoder
/// extension whitelist in `decoding::dispatch`.
pub const SUPPORTED_UTIS: &[UtiEntry] = &[
    // --- Standard formats ---
    UtiEntry {
        uti: "public.jpeg",
        label: "JPEG",
        detail: "*.jpg, *.jpeg",
    },
    UtiEntry {
        uti: "public.png",
        label: "PNG",
        detail: "*.png",
    },
    UtiEntry {
        uti: "com.compuserve.gif",
        label: "GIF",
        detail: "*.gif",
    },
    UtiEntry {
        uti: "public.webp",
        label: "WebP",
        detail: "*.webp",
    },
    UtiEntry {
        uti: "com.microsoft.bmp",
        label: "BMP",
        detail: "*.bmp",
    },
    UtiEntry {
        uti: "public.tiff",
        label: "TIFF",
        detail: "*.tiff, *.tif",
    },
    // --- Camera RAW formats ---
    UtiEntry {
        uti: "com.adobe.raw-image",
        label: "DNG",
        detail: "Universal",
    },
    UtiEntry {
        uti: "com.canon.cr2-raw-image",
        label: "CR2",
        detail: "Canon",
    },
    UtiEntry {
        uti: "com.canon.cr3-raw-image",
        label: "CR3",
        detail: "Canon",
    },
    UtiEntry {
        uti: "com.nikon.raw-image",
        label: "NEF",
        detail: "Nikon",
    },
    UtiEntry {
        uti: "com.sony.arw-raw-image",
        label: "ARW",
        detail: "Sony",
    },
    UtiEntry {
        uti: "com.olympus.or-raw-image",
        label: "ORF",
        detail: "Olympus",
    },
    UtiEntry {
        uti: "com.fuji.raw-image",
        label: "RAF",
        detail: "Fujifilm",
    },
    UtiEntry {
        uti: "com.panasonic.rw2-raw-image",
        label: "RW2",
        detail: "Panasonic",
    },
    UtiEntry {
        uti: "com.pentax.raw-image",
        label: "PEF",
        detail: "Pentax",
    },
    UtiEntry {
        uti: "com.samsung.raw-image",
        label: "SRW",
        detail: "Samsung",
    },
];

/// Standard image formats (JPEG, PNG, GIF, WebP, BMP, TIFF). A prefix slice of
/// [`SUPPORTED_UTIS`] — changing the order there requires updating these bounds.
pub const SUPPORTED_STANDARD_UTIS: &[UtiEntry] = SUPPORTED_UTIS.split_at(STANDARD_COUNT).0;

/// Camera RAW formats. A suffix slice of [`SUPPORTED_UTIS`] — see the caveat on
/// [`SUPPORTED_STANDARD_UTIS`].
pub const SUPPORTED_RAW_UTIS: &[UtiEntry] = SUPPORTED_UTIS.split_at(STANDARD_COUNT).1;

/// Tri-state summary of a group of per-UTI toggles.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GroupState {
    /// Every UTI in the group has Prvw as the default handler.
    All,
    /// No UTI in the group has Prvw as the default handler.
    None,
    /// Some, but not all, UTIs in the group have Prvw as the default handler.
    Mixed,
}

impl GroupState {
    /// Compute the tri-state summary from the per-UTI booleans in a group.
    ///
    /// An empty group reports `All` because there's nothing contradicting "all set".
    /// This never matters in practice (both real groups are non-empty) but keeps the
    /// function total.
    pub fn from_flags(flags: &[bool]) -> Self {
        let on_count = flags.iter().filter(|&&b| b).count();
        if on_count == flags.len() {
            Self::All
        } else if on_count == 0 {
            Self::None
        } else {
            Self::Mixed
        }
    }
}

/// Returns true if the running binary is inside a `.app` bundle.
pub fn is_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.contains(".app/Contents/MacOS/")))
        .unwrap_or(false)
}

/// Returns true if the `.app` bundle is in /Applications.
pub fn is_in_applications() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.starts_with("/Applications/")))
        .unwrap_or(false)
}

/// Gets the bundle ID of the current default handler for a UTI.
/// Returns None if no handler is set.
#[allow(deprecated)]
pub fn get_handler_bundle_id(uti: &str) -> Option<String> {
    let cf_uti = CFString::from_str(uti);
    let handler = unsafe { LSCopyDefaultRoleHandlerForContentType(&cf_uti, LSRolesMask::All) };
    handler.map(|h| h.to_string())
}

/// Convert a bundle ID like "com.apple.Preview" to a display name like "Preview.app".
pub fn bundle_id_to_app_name(bundle_id: &str) -> String {
    let app_name = bundle_id.rsplit('.').next().unwrap_or(bundle_id);
    format!("{app_name}.app")
}

/// Returns true if Prvw is the current default handler for the given UTI.
pub fn is_prvw_default(uti: &str) -> bool {
    get_handler_bundle_id(uti)
        .map(|id| id.contains("prvw") || id.contains("Prvw"))
        .unwrap_or(false)
}

/// Set Prvw as the default handler for a single UTI.
/// Saves the previous handler to settings before overwriting.
#[allow(deprecated)]
pub fn set_prvw_as_handler(uti: &str) {
    // Save the current handler before overwriting
    if let Some(current) = get_handler_bundle_id(uti)
        && !current.contains("prvw")
        && !current.contains("Prvw")
    {
        let mut settings = crate::settings::Settings::load();
        settings
            .previous_handlers
            .entry(uti.to_string())
            .or_insert(current);
        settings.save();
    }

    let cf_bundle_id = CFString::from_str(BUNDLE_ID);
    let cf_uti = CFString::from_str(uti);
    let status =
        unsafe { LSSetDefaultRoleHandlerForContentType(&cf_uti, LSRolesMask::All, &cf_bundle_id) };
    if status != 0 {
        log::warn!("Couldn't set handler for {uti}: OSStatus {status}");
    }
}

/// Restore the previous handler for a UTI. Falls back to Preview.app if no previous
/// handler is stored (e.g., upgraded from an older version without this feature).
#[allow(deprecated)]
pub fn restore_handler(uti: &str) {
    let settings = crate::settings::Settings::load();
    let previous = settings
        .previous_handlers
        .get(uti)
        .cloned()
        .unwrap_or_else(|| DEFAULT_FALLBACK.to_string());

    let cf_bundle_id = CFString::from_str(&previous);
    let cf_uti = CFString::from_str(uti);
    let status =
        unsafe { LSSetDefaultRoleHandlerForContentType(&cf_uti, LSRolesMask::All, &cf_bundle_id) };
    if status != 0 {
        log::warn!("Couldn't restore handler for {uti} to {previous}: OSStatus {status}");
    }
}

/// Sets Prvw as the default handler for every supported image UTI (standard + RAW).
pub fn set_as_default_viewer() {
    for entry in SUPPORTED_UTIS {
        set_prvw_as_handler(entry.uti);
    }
    log::info!("Set Prvw as default viewer for all supported image types");
}

/// Short, human-readable summary of current handlers for JPEG and PNG. Used as the
/// onboarding window's "Current defaults" line — we only show the two flagship formats
/// to keep the window compact.
pub fn query_handler_status() -> String {
    let mut lines = String::new();
    for entry in SUPPORTED_STANDARD_UTIS.iter().take(2) {
        let handler = get_handler_bundle_id(entry.uti)
            .map(|id| bundle_id_to_app_name(&id))
            .unwrap_or_else(|| "unknown".to_string());
        let marker = if is_prvw_default(entry.uti) {
            " (you)"
        } else {
            ""
        };
        lines.push_str(&format!("  {}: {handler}{marker}\n", entry.label));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_slices_total_matches_combined_list() {
        assert_eq!(SUPPORTED_STANDARD_UTIS.len(), STANDARD_COUNT);
        assert_eq!(SUPPORTED_RAW_UTIS.len(), 10);
        assert_eq!(
            SUPPORTED_UTIS.len(),
            SUPPORTED_STANDARD_UTIS.len() + SUPPORTED_RAW_UTIS.len()
        );
    }

    #[test]
    fn standard_utis_cover_expected_formats() {
        let expected = [
            "public.jpeg",
            "public.png",
            "com.compuserve.gif",
            "public.webp",
            "com.microsoft.bmp",
            "public.tiff",
        ];
        assert_eq!(SUPPORTED_STANDARD_UTIS.len(), expected.len());
        for (entry, want) in SUPPORTED_STANDARD_UTIS.iter().zip(expected) {
            assert_eq!(entry.uti, want);
        }
    }

    #[test]
    fn raw_utis_cover_expected_formats() {
        let expected = [
            "com.adobe.raw-image",
            "com.canon.cr2-raw-image",
            "com.canon.cr3-raw-image",
            "com.nikon.raw-image",
            "com.sony.arw-raw-image",
            "com.olympus.or-raw-image",
            "com.fuji.raw-image",
            "com.panasonic.rw2-raw-image",
            "com.pentax.raw-image",
            "com.samsung.raw-image",
        ];
        assert_eq!(SUPPORTED_RAW_UTIS.len(), expected.len());
        for (entry, want) in SUPPORTED_RAW_UTIS.iter().zip(expected) {
            assert_eq!(entry.uti, want);
        }
    }

    #[test]
    fn supported_utis_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for entry in SUPPORTED_UTIS {
            assert!(seen.insert(entry.uti), "duplicate UTI: {}", entry.uti);
        }
        assert_eq!(seen.len(), 16);
    }

    #[test]
    fn group_state_all_when_every_flag_on() {
        assert_eq!(GroupState::from_flags(&[true, true, true]), GroupState::All);
    }

    #[test]
    fn group_state_none_when_every_flag_off() {
        assert_eq!(
            GroupState::from_flags(&[false, false, false]),
            GroupState::None
        );
    }

    #[test]
    fn group_state_mixed_when_some_flags_on() {
        assert_eq!(
            GroupState::from_flags(&[true, false, true]),
            GroupState::Mixed
        );
        assert_eq!(GroupState::from_flags(&[true, false]), GroupState::Mixed);
    }

    #[test]
    fn group_state_empty_reports_all() {
        // Both real groups are non-empty, but keep the helper total.
        assert_eq!(GroupState::from_flags(&[]), GroupState::All);
    }
}
