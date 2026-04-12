//! Background auto-updater for macOS.
//!
//! On startup, fetches `latest.json` from getprvw.com, compares versions, and if a newer
//! version is available, downloads the DMG, mounts it, and replaces the running `.app` bundle.
//! Runs on a background `std::thread` so it never blocks the UI.

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST_URL: &str = "https://getprvw.com/latest.json";
const MOUNT_POINT: &str = "/tmp/prvw-update-mount";

#[derive(Debug, Deserialize)]
struct UpdateManifest {
    version: String,
    platforms: HashMap<String, PlatformEntry>,
}

#[derive(Debug, Deserialize)]
struct PlatformEntry {
    url: String,
}

/// Spawns a background thread that checks for updates, downloads, and installs if available.
/// Never blocks the calling thread. All errors are logged as warnings, never panics.
pub fn check_and_update() {
    if let Err(e) = std::thread::Builder::new()
        .name("updater".into())
        .spawn(|| {
            if let Err(e) = run_update() {
                log::warn!("Update check failed: {e}");
            }
        })
    {
        log::warn!("Couldn't spawn updater thread: {e}");
    }
}

fn run_update() -> Result<(), String> {
    // Skip in CI
    if std::env::var("CI").is_ok() {
        log::debug!("Skipping update check in CI");
        return Ok(());
    }

    // Skip if not running from a .app bundle (dev builds)
    let bundle_path = match find_running_bundle() {
        Some(p) => p,
        None => {
            log::debug!("Not running from a .app bundle, skipping update check");
            return Ok(());
        }
    };

    let current_version = env!("CARGO_PKG_VERSION");
    log::info!("Checking for updates (current: v{current_version})");

    // Fetch manifest
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Couldn't create HTTP client: {e}"))?;

    let manifest: UpdateManifest = client
        .get(MANIFEST_URL)
        .send()
        .map_err(|e| format!("Couldn't fetch update manifest: {e}"))?
        .json()
        .map_err(|e| format!("Couldn't parse update manifest: {e}"))?;

    // Compare versions
    if !is_newer(&manifest.version, current_version) {
        log::debug!("No update available (latest: v{})", manifest.version);
        return Ok(());
    }

    log::info!(
        "Update available: v{} (current: v{current_version})",
        manifest.version
    );

    // Resolve platform
    let platform_key = format!("darwin-{}", std::env::consts::ARCH);
    let entry = manifest
        .platforms
        .get(&platform_key)
        .ok_or_else(|| format!("No update available for platform {platform_key}"))?;

    let url = &entry.url;
    log::info!("Downloading update from {url}...");

    // Download DMG to a temp file
    let dmg_bytes = client
        .get(url)
        .send()
        .map_err(|e| format!("Couldn't download update: {e}"))?
        .bytes()
        .map_err(|e| format!("Couldn't read update response: {e}"))?;

    let temp_dir = std::env::temp_dir().join("prvw-update");
    fs::create_dir_all(&temp_dir).map_err(|e| format!("Couldn't create temp dir: {e}"))?;
    let dmg_path = temp_dir.join("update.dmg");
    fs::write(&dmg_path, &dmg_bytes).map_err(|e| format!("Couldn't write DMG: {e}"))?;

    // Mount DMG
    let mount_point = Path::new(MOUNT_POINT);
    // Ensure any stale mount is cleaned up
    if mount_point.exists() {
        let _ = Command::new("hdiutil")
            .args(["detach", MOUNT_POINT, "-force"])
            .output();
    }

    let mount_output = Command::new("hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-mountpoint",
            MOUNT_POINT,
        ])
        .arg(&dmg_path)
        .output()
        .map_err(|e| format!("Couldn't run hdiutil attach: {e}"))?;

    if !mount_output.status.success() {
        let stderr = String::from_utf8_lossy(&mount_output.stderr);
        return Err(format!("hdiutil attach failed: {stderr}"));
    }

    // Find Prvw.app in the mounted DMG
    let mounted_app = mount_point.join("Prvw.app");
    if !mounted_app.exists() {
        detach_dmg();
        return Err("Mounted DMG doesn't contain Prvw.app".to_string());
    }

    // Replace the .app bundle: copy to temp location next to the bundle, then rename
    let result = replace_app_bundle(&mounted_app, &bundle_path);

    // Always unmount
    detach_dmg();

    // Clean up temp files
    let _ = fs::remove_dir_all(&temp_dir);

    result?;

    log::info!(
        "Update installed: v{}. Restart to use it.",
        manifest.version
    );
    Ok(())
}

/// Replaces the running .app bundle with the new one from the mounted DMG.
/// Uses atomic approach: cp -R to a temp location, then rename over the original.
/// Falls back to osascript with admin privileges if direct copy fails with permission denied.
fn replace_app_bundle(source_app: &Path, dest_app: &Path) -> Result<(), String> {
    let parent = dest_app
        .parent()
        .ok_or_else(|| "Couldn't determine parent directory of .app bundle".to_string())?;
    let temp_app = parent.join("Prvw.app.prvw-update-tmp");

    // Clean up any previous temp
    if temp_app.exists() {
        let _ = fs::remove_dir_all(&temp_app);
    }

    // Try direct copy first
    match copy_app_recursive(source_app, &temp_app) {
        Ok(()) => match fs::rename(&temp_app, dest_app) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_dir_all(&temp_app);
                if is_permission_error(&e.to_string()) {
                    log::info!("Direct rename denied, escalating with admin privileges");
                    copy_with_admin_privileges(source_app, dest_app)
                } else {
                    Err(format!("Couldn't rename temp app to destination: {e}"))
                }
            }
        },
        Err(e) => {
            let _ = fs::remove_dir_all(&temp_app);
            if is_permission_error(&e) {
                log::info!("Direct copy denied, escalating with admin privileges");
                copy_with_admin_privileges(source_app, dest_app)
            } else {
                Err(e)
            }
        }
    }
}

/// Recursively copies a directory tree using `cp -R`.
fn copy_app_recursive(src: &Path, dest: &Path) -> Result<(), String> {
    let output = Command::new("cp")
        .args(["-R"])
        .arg(src)
        .arg(dest)
        .output()
        .map_err(|e| format!("Couldn't run cp: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("cp -R failed: {stderr}"));
    }
    Ok(())
}

/// Copies the .app bundle using osascript with admin privileges.
fn copy_with_admin_privileges(source_app: &Path, dest_app: &Path) -> Result<(), String> {
    // Remove old, then copy new -- both in one admin command
    let script = format!(
        "do shell script \"rm -rf '{}' && cp -R '{}' '{}'\" with administrator privileges",
        dest_app.display(),
        source_app.display(),
        dest_app.display()
    );

    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("Couldn't run osascript: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Admin copy failed: {stderr}"));
    }

    Ok(())
}

/// Finds the running app's `.app` bundle path by walking up from `current_exe()`.
/// Returns `None` if the binary isn't inside a `.app` bundle (dev builds).
fn find_running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut path = exe.as_path();
    while let Some(parent) = path.parent() {
        if path.extension().is_some_and(|ext| ext == "app") {
            return Some(path.to_path_buf());
        }
        path = parent;
    }
    None
}

/// Simple semver comparison: returns true if `remote` is newer than `current`.
/// Parses "major.minor.patch" and compares numerically.
fn is_newer(remote: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<(u32, u32, u32)> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    };

    match (parse(remote), parse(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

fn is_permission_error(error: &str) -> bool {
    error.contains("Permission denied") || error.contains("Operation not permitted")
}

fn detach_dmg() {
    if let Err(e) = Command::new("hdiutil")
        .args(["detach", MOUNT_POINT, "-force"])
        .output()
    {
        log::warn!("Couldn't detach DMG: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("1.1.0", "1.0.9"));
        assert!(is_newer("2.0.0", "1.9.9"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.0", "1.0.0"));
        assert!(!is_newer("invalid", "1.0.0"));
    }
}
