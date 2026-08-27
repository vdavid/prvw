//! Installing an update on macOS: download the DMG, mount it, and swap the running bundle.
//!
//! ## Gotchas
//!
//! - **Bundle replacement is atomic via `renamex_np(RENAME_SWAP)`** on the non-admin path.
//!   We copy the new bundle to a sibling temp path, then swap it with the running bundle in
//!   a single syscall, so there's no window where `/Applications/Prvw.app` is absent or
//!   partial. The admin-escalation fallback still uses rm+cp via `osascript` because
//!   `renamex_np` can't be invoked cleanly through an AppleScript shell; that path is rarely
//!   hit (only when `/Applications` isn't user-writable).
//! - **Launch Services is told to forget the old bundle before the swap** (`lsregister -u`)
//!   and to re-register `dest_app` unconditionally afterward (`lsregister -f`). Without the
//!   `-u`, LS often keeps the pre-swap registration alive and surfaces both versions in the
//!   "Open With" menu. The final `-f` runs on success and failure paths: on failure it
//!   harmlessly re-registers the still-present old bundle, so we never leave LS in an
//!   "app forgotten, bundle still exists" limbo.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MOUNT_POINT: &str = "/tmp/prvw-update-mount";

/// Download `url` and put it in place of the running bundle. `version` is only for the log.
pub fn install(version: &str, url: &str) -> Result<(), String> {
    // `is_in_applications()` already verified the prefix; this just resolves the exact path.
    let bundle_path =
        find_running_bundle().ok_or_else(|| "Couldn't find running .app bundle".to_string())?;

    log::info!("Downloading update from {url}...");

    // Download DMG to a temp file
    let download_client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("Couldn't create HTTP client: {e}"))?;
    let dmg_bytes = download_client
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

    log::info!("Update installed: v{version}. Restart to use it.");
    Ok(())
}

/// Replaces the running .app bundle with the new one from the mounted DMG.
///
/// Copies the new bundle to a sibling `Prvw.app.prvw-update-new` path, then swaps it with
/// the running bundle via `renamex_np(RENAME_SWAP)`, a single kernel-atomic operation.
/// After the swap, the sibling path holds the old bundle, which we remove best-effort.
///
/// Also coordinates with Launch Services: unregisters the old bundle right before the swap
/// so LS doesn't end up with two records for the same path (the "Open With menu shows both
/// 0.6.3 and 0.10.0" class of bug), and re-registers whatever ends up at `dest_app` at the
/// very end: idempotent on success, restorative on failure.
///
/// Falls back to `osascript` with admin privileges if either the copy or the swap fails
/// with a permission error, typically when `/Applications` isn't user-writable on a
/// managed Mac.
fn replace_app_bundle(source_app: &Path, dest_app: &Path) -> Result<(), String> {
    let result = replace_app_bundle_inner(source_app, dest_app);

    // Always re-register whatever is at `dest_app` with Launch Services. On success this
    // ensures LS re-reads the new Info.plist (needed to pick up document-type additions
    // from the update). On failure this restores LS's view of the still-present old
    // bundle, in case we got as far as calling `lsregister -u` before the swap failed.
    register_with_launch_services(dest_app);

    result
}

fn replace_app_bundle_inner(source_app: &Path, dest_app: &Path) -> Result<(), String> {
    let parent = dest_app
        .parent()
        .ok_or_else(|| "Couldn't determine parent directory of .app bundle".to_string())?;
    // Deliberately not ending in `.app` so Launch Services doesn't index this as a bundle
    // during the brief window it exists.
    let staging = parent.join("Prvw.app.prvw-update-new");

    // Clean up any leftover staging dir from a previous aborted run.
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }

    if let Err(e) = copy_app_recursive(source_app, &staging) {
        let _ = fs::remove_dir_all(&staging);
        if is_permission_error(&e) {
            log::info!("Direct copy denied, escalating with admin privileges");
            return copy_with_admin_privileges(source_app, dest_app);
        }
        return Err(e);
    }

    // Forget the old bundle in Launch Services before we replace it. Without this, LS
    // often keeps the old registration alive post-swap (inode changed, LS didn't notice)
    // and surfaces both versions in the "Open With" menu until the LS database is
    // rebuilt. The outer `replace_app_bundle` re-registers `dest_app` unconditionally
    // once this function returns, so a failure after this point is self-healing.
    unregister_from_launch_services(dest_app);

    // Atomic swap. After this, `dest_app` holds the new bundle and `staging` holds the old.
    if let Err(e) = atomic_swap(&staging, dest_app) {
        let _ = fs::remove_dir_all(&staging);
        if is_permission_error(&e) {
            log::info!("Atomic swap denied, escalating with admin privileges");
            return copy_with_admin_privileges(source_app, dest_app);
        }
        return Err(e);
    }

    // Remove the old bundle (now at the staging path). Best-effort: the update itself
    // already succeeded at this point. Leaving it around is harmless because the path
    // doesn't end in `.app`, so Launch Services won't surface it.
    if let Err(e) = fs::remove_dir_all(&staging) {
        log::warn!("Couldn't remove old bundle at {}: {e}", staging.display());
    }

    Ok(())
}

const LSREGISTER_PATH: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// Tells Launch Services to drop any registration for the bundle at `path`. Best-effort:
/// a failure here isn't fatal, it just means the user might see a stale entry in
/// "Open With" until LS's next scan.
fn unregister_from_launch_services(path: &Path) {
    let _ = Command::new(LSREGISTER_PATH)
        .args(["-u"])
        .arg(path)
        .output();
}

/// Tells Launch Services to (re-)register the bundle at `path`. Best-effort.
/// `-f` forces a re-read of Info.plist so new document types added in an update are
/// picked up.
fn register_with_launch_services(path: &Path) {
    let _ = Command::new(LSREGISTER_PATH)
        .args(["-f"])
        .arg(path)
        .output();
}

/// Atomically swaps two existing filesystem paths via `renamex_np(RENAME_SWAP)`.
/// Both paths must exist on the same filesystem. Requires macOS 10.12+ (always true for
/// Prvw's supported targets).
fn atomic_swap(a: &Path, b: &Path) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let a_c = CString::new(a.as_os_str().as_bytes())
        .map_err(|e| format!("Invalid path for swap ({}): {e}", a.display()))?;
    let b_c = CString::new(b.as_os_str().as_bytes())
        .map_err(|e| format!("Invalid path for swap ({}): {e}", b.display()))?;

    let rc = unsafe { libc::renamex_np(a_c.as_ptr(), b_c.as_ptr(), libc::RENAME_SWAP) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!("renamex_np(RENAME_SWAP) failed: {err}"));
    }
    Ok(())
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
    fn atomic_swap_exchanges_directory_contents() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        fs::write(a.join("marker"), "from-a").unwrap();
        fs::write(b.join("marker"), "from-b").unwrap();

        atomic_swap(&a, &b).expect("swap");

        assert_eq!(fs::read_to_string(a.join("marker")).unwrap(), "from-b");
        assert_eq!(fs::read_to_string(b.join("marker")).unwrap(), "from-a");
    }

    #[test]
    fn atomic_swap_fails_when_path_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = tmp.path().join("exists");
        let b = tmp.path().join("does-not-exist");
        fs::create_dir(&a).unwrap();

        assert!(atomic_swap(&a, &b).is_err());
    }
}
