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

/// UTIs for all image types Prvw supports, with human-readable labels and file extensions.
pub const SUPPORTED_UTIS: &[UtiEntry] = &[
    UtiEntry {
        uti: "public.jpeg",
        label: "JPEG",
        extensions: "*.jpg, *.jpeg",
    },
    UtiEntry {
        uti: "public.png",
        label: "PNG",
        extensions: "*.png",
    },
    UtiEntry {
        uti: "com.compuserve.gif",
        label: "GIF",
        extensions: "*.gif",
    },
    UtiEntry {
        uti: "public.tiff",
        label: "TIFF",
        extensions: "*.tiff, *.tif",
    },
    UtiEntry {
        uti: "com.microsoft.bmp",
        label: "BMP",
        extensions: "*.bmp",
    },
    UtiEntry {
        uti: "public.webp",
        label: "WebP",
        extensions: "*.webp",
    },
];

pub struct UtiEntry {
    pub uti: &'static str,
    pub label: &'static str,
    pub extensions: &'static str,
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

/// Sets Prvw as the default handler for ALL supported image types.
/// Saves previous handlers before overwriting.
pub fn set_as_default_viewer() {
    for entry in SUPPORTED_UTIS {
        set_prvw_as_handler(entry.uti);
    }
    log::info!("Set Prvw as default viewer for all supported image types");
}

/// Get the display name of the handler the user had before Prvw, or a default message.
pub fn previous_handler_name(uti: &str) -> String {
    let settings = crate::settings::Settings::load();
    match settings.previous_handlers.get(uti) {
        Some(bundle_id) => bundle_id_to_app_name(bundle_id),
        None => "Preview.app (macOS default)".to_string(),
    }
}

/// Query current file association status for JPEG and PNG (for the onboarding window).
/// Returns a human-readable multiline string.
pub fn query_handler_status() -> String {
    let mut lines = String::new();
    for entry in &SUPPORTED_UTIS[..2] {
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
