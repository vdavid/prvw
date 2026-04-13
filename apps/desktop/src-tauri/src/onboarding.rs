//! Onboarding helpers for macOS.
//!
//! Provides app bundle detection, file association queries, and the ability to set
//! Prvw as the default image viewer. The onboarding UI itself is a Dioxus component
//! in `main.rs`.

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
#[allow(dead_code)] // Used by onboarding flow when launched from .app bundle
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
pub fn set_as_default_viewer() -> Result<(), String> {
    let utis: Vec<&str> = SUPPORTED_UTIS.iter().map(|(uti, _)| *uti).collect();
    let uti_array = utis
        .iter()
        .map(|u| format!("\"{u}\""))
        .collect::<Vec<_>>()
        .join(", ");

    let script = format!(
        r#"
import CoreServices
let bundleID = "com.veszelovszki.prvw" as! CFString
let types: [String] = [{uti_array}]
var failed: [String] = []
for uti in types {{
    let status = LSSetDefaultRoleHandlerForContentType(uti as! CFString, LSRolesMask.all, bundleID)
    if status != 0 {{
        failed.append("\(uti) (status \(status))")
    }}
}}
if failed.isEmpty {{
    print("ok", terminator: "")
}} else {{
    print("FAILED: " + failed.joined(separator: ", "), terminator: "")
}}
"#
    );

    let output = Command::new("swift")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("Couldn't run swift: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = format!("Swift script failed: {stderr}");
        log::error!("{msg}");
        return Err(msg);
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.starts_with("FAILED") {
        log::error!("Failed to set default viewer: {stdout}");
        return Err(stdout);
    }

    log::info!("Set Prvw as default viewer for all supported image types");
    Ok(())
}

/// Query current file association status for a couple of representative types.
/// Returns a human-readable multiline string.
pub fn query_handler_status() -> String {
    let mut lines = String::new();
    // Only query JPEG and PNG to avoid slow startup (each swift invocation takes ~0.5s)
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
