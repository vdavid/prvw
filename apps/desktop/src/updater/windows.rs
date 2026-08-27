//! Telling a Windows user that a newer Prvw is out.
//!
//! Windows has no self-updater: the app ships as an NSIS installer that wants a person to
//! click through it, so the useful end of a check is the download, in the browser they
//! already trust. `ShellExecuteW` hands the URL to the shell and returns, which is the whole
//! reason to prefer it over any window of our own: a dialog needs a message loop, and a
//! nested one starves winit's pump (see the gotcha in `AGENTS.md`).
//!
//! **A version is announced once.** `update-announced` in the app data directory holds the
//! last version we opened a page for, so someone who looks and decides to stay where they are
//! doesn't meet the page again on every launch. Losing the file (a cleared profile, a
//! `PRVW_DATA_DIR` that moved) costs one extra tab, which is why it's a plain file rather
//! than a settings field: nothing about it is worth a schema.

use std::path::PathBuf;

use super::manifest;

/// Where to send someone when the manifest carries no file for this platform yet. A release
/// that has published its Mac builds but not its Windows installer is still worth knowing
/// about, and the site is where the installer will turn up.
const DOWNLOAD_PAGE: &str = "https://getprvw.com/";

/// Open the download for `version`, unless we already did. `url` is the installer named by the
/// manifest, absent when this release hasn't published one for Windows.
pub fn announce(version: &str, url: Option<&str>) -> Result<(), String> {
    let last_announced = std::fs::read_to_string(announced_path()).ok();
    let last_announced = last_announced.as_deref().map(str::trim);
    if !manifest::should_announce(version, last_announced) {
        log::debug!("Update v{version} is available, and we've already pointed at it");
        return Ok(());
    }

    let url = url.unwrap_or(DOWNLOAD_PAGE);
    log::info!("Update available: v{version}. Opening {url}");
    crate::platform::windows::open_url(url)?;

    // Only once the browser has it, so a shell that refused gets another try next launch.
    remember_announced(version);
    Ok(())
}

/// The file holding the last version we opened a page for.
fn announced_path() -> PathBuf {
    crate::settings::persistence::data_dir().join("update-announced")
}

fn remember_announced(version: &str) {
    let path = announced_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, version) {
        log::warn!(
            "Couldn't record the announced update at {}: {e}",
            path.display()
        );
    }
}
