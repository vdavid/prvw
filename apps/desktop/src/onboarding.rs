//! Onboarding screen rendered with wgpu + glyphon.
//!
//! When Prvw is launched without file arguments from a `.app` bundle (for example, by
//! double-clicking in Finder), shows a welcome screen in the main window with text
//! explaining how to use the app and an option to set Prvw as the default image viewer.

use crate::text::TextBlock;
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
pub fn set_as_default_viewer() {
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
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::error!("Failed to set default viewer: {stderr}");
            }
        }
        Err(e) => log::error!("Couldn't run swift to set default viewer: {e}"),
    }
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

/// Build the text blocks for the onboarding screen.
pub fn onboarding_text_blocks(screen_width: u32, screen_height: u32) -> Vec<TextBlock> {
    let version = env!("CARGO_PKG_VERSION");
    let handler_status = query_handler_status();

    let location_tip = if !is_in_applications() {
        "\nTip: move Prvw.app to /Applications for the best experience.\n"
    } else {
        ""
    };

    let padding = 40.0;
    let max_width = (screen_width as f32 - padding * 2.0).max(200.0);
    let center_x = padding;

    // Light text color for dark background
    let title_color: [u8; 4] = [255, 255, 255, 255];
    let body_color: [u8; 4] = [200, 200, 210, 255];
    let dim_color: [u8; 4] = [140, 140, 155, 255];

    let mut blocks = Vec::new();
    let mut y = 50.0;

    // Title
    blocks.push(TextBlock {
        text: format!("Prvw v{version}"),
        x: center_x,
        y,
        font_size: 28.0,
        line_height: 36.0,
        color: title_color,
        max_width: Some(max_width),
    });
    y += 48.0;

    // Subtitle
    blocks.push(TextBlock {
        text: "A fast image viewer for macOS.".to_string(),
        x: center_x,
        y,
        font_size: 16.0,
        line_height: 22.0,
        color: body_color,
        max_width: Some(max_width),
    });
    y += 40.0;

    // Usage instructions
    blocks.push(TextBlock {
        text: "To view images, right-click any image and choose\n\"Open With\" > \"Prvw\"."
            .to_string(),
        x: center_x,
        y,
        font_size: 14.0,
        line_height: 20.0,
        color: body_color,
        max_width: Some(max_width),
    });
    y += 56.0;

    // Current handlers
    blocks.push(TextBlock {
        text: format!("Current defaults:\n{handler_status}"),
        x: center_x,
        y,
        font_size: 13.0,
        line_height: 18.0,
        color: dim_color,
        max_width: Some(max_width),
    });
    y += 72.0;

    // Location tip
    if !location_tip.is_empty() {
        blocks.push(TextBlock {
            text: location_tip.trim().to_string(),
            x: center_x,
            y,
            font_size: 13.0,
            line_height: 18.0,
            color: dim_color,
            max_width: Some(max_width),
        });
        y += 30.0;
    }

    // Action instructions at the bottom
    let bottom_y = (screen_height as f32 - 50.0).max(y + 20.0);
    blocks.push(TextBlock {
        text: "Press Enter to set as default viewer, or Escape to close.".to_string(),
        x: center_x,
        y: bottom_y,
        font_size: 14.0,
        line_height: 20.0,
        color: title_color,
        max_width: Some(max_width),
    });

    blocks
}
