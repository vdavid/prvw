//! Onboarding helpers: file association queries and default viewer registration.
//!
//! When Prvw is launched without file arguments, the native onboarding window
//! (in `native_ui.rs`) uses these functions to show the current handler status
//! and let the user set Prvw as the default image viewer.
//!
//! Uses CoreServices FFI directly (via objc2-core-services) instead of shelling
//! out to `swift -e`. This is faster (~instant vs ~0.5s per swift invocation)
//! and doesn't depend on the Swift toolchain at runtime.

// These CoreServices functions are deprecated in favor of NSWorkspace async methods,
// but the sync versions are simpler and work fine for our use case.
use objc2_core_foundation::CFString;
#[allow(deprecated)]
use objc2_core_services::{
    LSCopyDefaultRoleHandlerForContentType, LSRolesMask, LSSetDefaultRoleHandlerForContentType,
};

const BUNDLE_ID: &str = "com.veszelovszki.prvw";

/// UTIs for all image types Prvw supports.
const SUPPORTED_UTIS: &[(&str, &str)] = &[
    ("public.jpeg", "JPEG"),
    ("public.png", "PNG"),
    ("com.compuserve.gif", "GIF"),
    ("public.tiff", "TIFF"),
    ("com.microsoft.bmp", "BMP"),
    ("public.webp", "WebP"),
];

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

/// Gets the bundle identifier of the default handler for a given UTI.
/// Returns the app name (for example, "Preview.app") or "unknown" on failure.
#[allow(deprecated)] // LSCopyDefaultRoleHandlerForContentType — sync is fine here
fn get_default_handler(uti: &str) -> String {
    let cf_uti = CFString::from_str(uti);
    let handler = unsafe { LSCopyDefaultRoleHandlerForContentType(&cf_uti, LSRolesMask::All) };
    match handler {
        Some(bundle_id) => {
            let id = bundle_id.to_string();
            // Convert bundle ID like "com.apple.Preview" to "Preview.app"
            let app_name = id.rsplit('.').next().unwrap_or(&id);
            format!("{app_name}.app")
        }
        None => "unknown".to_string(),
    }
}

/// Sets Prvw as the default handler for all supported image types.
#[allow(deprecated)] // LSSetDefaultRoleHandlerForContentType — sync is fine here
pub fn set_as_default_viewer() {
    let cf_bundle_id = CFString::from_str(BUNDLE_ID);

    for &(uti, label) in SUPPORTED_UTIS {
        let cf_uti = CFString::from_str(uti);
        let status = unsafe {
            LSSetDefaultRoleHandlerForContentType(&cf_uti, LSRolesMask::All, &cf_bundle_id)
        };
        if status != 0 {
            log::warn!("Couldn't set handler for {label} ({uti}): OSStatus {status}");
        }
    }

    log::info!("Set Prvw as default viewer for all supported image types");
}

/// Query current file association status for JPEG and PNG.
/// Returns a human-readable multiline string.
pub fn query_handler_status() -> String {
    let mut lines = String::new();
    for &(uti, label) in &SUPPORTED_UTIS[..2] {
        let handler = get_default_handler(uti);
        let marker = if handler.contains("Prvw") || handler.contains("prvw") {
            " (you)"
        } else {
            ""
        };
        lines.push_str(&format!("  {label}: {handler}{marker}\n"));
    }
    lines
}
