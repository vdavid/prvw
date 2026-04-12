//! No-args onboarding dialog for macOS.
//!
//! When Prvw is launched without file arguments from a `.app` bundle (for example, by
//! double-clicking in Finder), shows a native dialog explaining how to use the app and
//! offers to set Prvw as the default image viewer.

use std::process::Command;

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
fn is_in_applications() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.starts_with("/Applications/")))
        .unwrap_or(false)
}

/// Gets the name of the default handler app for a given UTI (for example, "Preview.app").
/// Returns "unknown" on failure.
fn get_default_handler(uti: &str) -> String {
    let script = format!(
        r#"
import AppKit
import UniformTypeIdentifiers
if let uttype = UTType("{uti}"),
   let url = NSWorkspace.shared.urlForApplication(toOpen: uttype) {{
    print(url.lastPathComponent, terminator: "")
}} else {{
    print("unknown", terminator: "")
}}
"#
    );
    Command::new("swift")
        .args(["-e", &script])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Sets Prvw as the default handler for all supported image types.
/// Uses `swift -e` to call LSSetDefaultRoleHandlerForContentType.
fn set_as_default_viewer() {
    let utis: Vec<&str> = SUPPORTED_UTIS.iter().map(|(uti, _)| *uti).collect();
    let uti_array = utis
        .iter()
        .map(|u| format!("\"{u}\""))
        .collect::<Vec<_>>()
        .join(", ");

    let script = format!(
        r#"
import CoreServices
let bundleID = "com.veszelovszki.prvw" as CFString
let types: [String] = [{uti_array}]
for uti in types {{
    let status = LSSetDefaultRoleHandlerForContentType(uti as CFString, LSRolesMask.all, bundleID)
    if status != 0 {{
        print("Warning: failed to set handler for \(uti) (status \(status))")
    }}
}}
print("Done", terminator: "")
"#
    );

    match Command::new("swift").args(["-e", &script]).output() {
        Ok(output) => {
            if output.status.success() {
                log::info!("Set Prvw as default viewer for all supported image types");
                // Show confirmation dialog
                let _ = Command::new("osascript")
                    .args([
                        "-e",
                        "display dialog \"Prvw is now the default image viewer.\" \
                         with title \"Prvw\" buttons {\"OK\"} default button \"OK\" with icon note",
                    ])
                    .output();
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::error!("Failed to set default viewer: {stderr}");
            }
        }
        Err(e) => log::error!("Couldn't run swift to set default viewer: {e}"),
    }
}

/// Shows the onboarding dialog and handles the user's response.
/// Returns when the dialog is dismissed.
pub fn show_onboarding() {
    let version = env!("CARGO_PKG_VERSION");

    // Build file association state lines
    let mut handler_lines = String::new();
    // Only query a couple of representative types to avoid slow startup
    for &(uti, label) in &SUPPORTED_UTIS[..2] {
        let handler = get_default_handler(uti);
        handler_lines.push_str(&format!("  {label} files open with: {handler}\\n"));
    }

    let location_tip = if !is_in_applications() {
        "\\nTip: move Prvw.app to /Applications for the best experience.\\n"
    } else {
        ""
    };

    let message = format!(
        "Prvw v{version}\\n\
         \\n\
         A fast image viewer for macOS.\\n\
         \\n\
         To view images with Prvw, right-click any image\\n\
         and choose \\\"Open With\\\" > \\\"Prvw\\\".\\n\
         \\n\
         Current state:\\n\
         {handler_lines}\
         {location_tip}"
    );

    let script = format!(
        "set result to button returned of (display dialog \"{message}\" \
         with title \"Welcome to Prvw\" \
         buttons {{\"Close\", \"Set as default viewer\"}} \
         default button \"Close\" \
         with icon note)\n\
         return result"
    );

    match Command::new("osascript").args(["-e", &script]).output() {
        Ok(output) => {
            let button = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if button == "Set as default viewer" {
                set_as_default_viewer();
            }
        }
        Err(e) => log::error!("Couldn't show onboarding dialog: {e}"),
    }
}
