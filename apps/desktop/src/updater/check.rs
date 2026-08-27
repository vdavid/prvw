//! Running a check: fetch the manifest, ask [`super::manifest`] what it means, and hand the
//! answer to the platform that can act on it.
//!
//! Everything here is fire-and-forget on a background thread. If the manifest host is down we
//! log and move on: no user-visible error, no retries.

use super::manifest::{self, Outcome, UpdateManifest};

/// Override with PRVW_UPDATE_URL env var for testing.
fn manifest_url() -> String {
    std::env::var("PRVW_UPDATE_URL").unwrap_or_else(|_| "https://getprvw.com/latest.json".into())
}

/// Spawns a background thread that only *checks* for an update and logs the result.
/// Use this on launches where the user hasn't opened a file yet: we don't want to
/// download or prompt for admin privileges while they're staring at the onboarding.
#[cfg(target_os = "macos")]
pub fn check_only() {
    spawn("update-check", || {
        let Some(current_version) = start()? else {
            return Ok(());
        };
        match look()? {
            Outcome::UpToDate => Ok(()),
            Outcome::Available { version, .. } => {
                log::info!(
                    "Update available: v{version} (current: v{current_version}). Will install once a file is opened."
                );
                Ok(())
            }
        }
    });
}

/// Spawns the background check a launch runs, and acts on what it finds.
///
/// The two platforms with a delivery channel do different things with the same answer.
/// macOS downloads the DMG and swaps the running bundle in place. Windows ships as an
/// installer, so it opens the download in the browser instead and lets the person decide.
/// Never blocks the calling thread. All errors are logged as warnings, never panics.
pub fn check_on_launch() {
    spawn("updater", || {
        if start()?.is_none() {
            return Ok(());
        }
        match look()? {
            Outcome::UpToDate => Ok(()),
            Outcome::Available { version, url } => act(&version, url),
        }
    });
}

#[cfg(target_os = "macos")]
fn act(version: &str, url: Option<String>) -> Result<(), String> {
    let url = url.ok_or_else(|| {
        format!(
            "No update available for platform {}",
            manifest::host_platform_key()
        )
    })?;
    super::macos::install(version, &url)
}

#[cfg(target_os = "windows")]
fn act(version: &str, url: Option<String>) -> Result<(), String> {
    super::windows::announce(version, url.as_deref())
}

/// Run `work` on a named background thread, logging whatever it returns.
fn spawn(name: &str, work: impl FnOnce() -> Result<(), String> + Send + 'static) {
    if let Err(e) = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            if let Err(e) = work() {
                log::warn!("Update check failed: {e}");
            }
        })
    {
        log::warn!("Couldn't spawn {name} thread: {e}");
    }
}

/// The shared preamble. `None` means this session skips the check entirely; `Some` carries the
/// running version, which every log line downstream wants.
fn start() -> Result<Option<&'static str>, String> {
    if std::env::var("CI").is_ok() {
        log::debug!("Skipping update check in CI");
        return Ok(None);
    }
    // Only a copy that was installed checks. On macOS that's `/Applications`, so a dev build
    // or a copy sitting in `~/Downloads` never tries to update itself in place. Windows only
    // ever opens a browser, so it has nothing to protect against a portable copy; the one
    // thing to keep out is the build tree, where a check would put a download page in front of
    // whoever is working on the app (the E2E harness included).
    #[cfg(target_os = "macos")]
    if !crate::file_associations::is_in_applications() {
        log::debug!("Not installed in /Applications, skipping update check");
        return Ok(None);
    }
    #[cfg(target_os = "windows")]
    if cfg!(debug_assertions) {
        log::debug!("Dev build, skipping update check");
        return Ok(None);
    }

    let current_version = env!("CARGO_PKG_VERSION");
    log::info!("Checking for updates (current: v{current_version})");
    Ok(Some(current_version))
}

/// Fetch the manifest and say what it means for this build.
fn look() -> Result<Outcome, String> {
    let manifest = fetch_manifest()?;
    let outcome = manifest::evaluate(
        &manifest,
        env!("CARGO_PKG_VERSION"),
        &manifest::host_platform_key(),
    );
    if outcome == Outcome::UpToDate {
        log::debug!("No update available (latest: v{})", manifest.version);
    }
    Ok(outcome)
}

fn fetch_manifest() -> Result<UpdateManifest, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Couldn't create HTTP client: {e}"))?;

    let url = manifest_url();
    client
        .get(&url)
        .send()
        .map_err(|e| format!("Couldn't fetch update manifest: {e}"))?
        .json()
        .map_err(|e| format!("Couldn't parse update manifest: {e}"))
}
