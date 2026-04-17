//! Find a DCP matching the current camera.
//!
//! We search a prioritized list of directories and pick the first DCP whose
//! `UniqueCameraModel` matches (case-insensitive, whitespace-normalized)
//! the camera's identification string.
//!
//! ## Search paths (in order)
//!
//! 1. `$PRVW_DCP_DIR`: an optional env var pointing to a directory of user-
//!    provided DCPs. Highest priority so power users and tests can override
//!    the system default.
//! 2. `~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/`: the
//!    default install location for Adobe Camera Raw / Lightroom DCPs on
//!    macOS. Most users won't have ACR installed, in which case this
//!    directory won't exist and we silently move on.
//! 3. `~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/Adobe
//!    Standard/`: Adobe stashes its "Standard" profiles under a subfolder
//!    that ACR auto-discovers. We walk one level deep only (no global
//!    recursive scan) so decode stays snappy.
//!
//! ## Matching
//!
//! We match against the DNG spec's `UniqueCameraModel`, which Adobe writes
//! as `"<Make> <Model>"` (e.g., `"Sony ILCE-7M3"`). Rawler exposes the
//! camera's make and model separately, so we compose the same string and
//! compare. The match is case-insensitive and collapses runs of whitespace,
//! matching the "exact with whitespace tolerance" rule the DNG spec
//! recommends.
//!
//! If no DCP matches, we return `None`. Callers treat `None` as a no-op, so
//! the pipeline keeps running with the default color path — matching
//! Phase 3.1 output bit-for-bit.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::parser::{Dcp, parse};

/// Env var that overrides the default DCP search path. Useful for tests and
/// for users who keep their DCP library outside of Adobe's folder.
pub const DCP_DIR_ENV_VAR: &str = "PRVW_DCP_DIR";

/// Look up a DCP for the given camera identity. Returns `None` if no file
/// matches — the expected state for users without ACR installed.
///
/// `camera_id` is the value we match against each DCP's
/// `UniqueCameraModel`. Compose it at the call site from rawler's
/// `Camera.make` + `Camera.model` (exactly the DNG-spec form).
pub fn find_dcp_for_camera(camera_id: &str) -> Option<Dcp> {
    let target = normalize(camera_id);
    if target.is_empty() {
        return None;
    }
    for dir in search_dirs() {
        log::debug!("DCP: scanning {}", dir.display());
        match scan_dir_for_match(&dir, &target) {
            Ok(Some(dcp)) => {
                log::info!(
                    "DCP: matched '{}' for camera '{}' in {}",
                    dcp.profile_name.as_deref().unwrap_or("<unnamed profile>"),
                    camera_id,
                    dir.display()
                );
                if let Some(copyright) = dcp.profile_copyright.as_deref() {
                    log::debug!("DCP copyright: {copyright}");
                }
                return Some(dcp);
            }
            Ok(None) => {} // try next dir
            Err(e) => log::debug!("DCP: couldn't scan {}: {e}", dir.display()),
        }
    }
    if env::var_os(DCP_DIR_ENV_VAR).is_some() {
        log::info!(
            "DCP: no matching profile for camera '{camera_id}' in {}; falling back to default pipeline",
            DCP_DIR_ENV_VAR
        );
    } else {
        log::debug!("DCP: no match for camera '{camera_id}'; falling back to default pipeline");
    }
    None
}

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(env_dir) = env::var(DCP_DIR_ENV_VAR)
        && !env_dir.is_empty()
    {
        dirs.push(PathBuf::from(env_dir));
    }
    if let Some(home) = home_dir() {
        let adobe = home
            .join("Library")
            .join("Application Support")
            .join("Adobe")
            .join("CameraRaw")
            .join("CameraProfiles");
        dirs.push(adobe.clone());
        dirs.push(adobe.join("Adobe Standard"));
    }
    dirs
}

fn home_dir() -> Option<PathBuf> {
    // `std::env::home_dir` was marked deprecated for Windows quirks; we use
    // `HOME` directly since Prvw is macOS-only and `HOME` is always set by
    // launchd.
    env::var_os("HOME").map(PathBuf::from)
}

fn scan_dir_for_match(dir: &Path, target: &str) -> Result<Option<Dcp>, String> {
    if !dir.exists() {
        return Ok(None);
    }
    let entries = fs::read_dir(dir).map_err(|e| format!("read_dir: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            != Some("dcp".to_string())
        {
            continue;
        }
        match load_and_match(&path, target) {
            Ok(Some(dcp)) => return Ok(Some(dcp)),
            Ok(None) => {}
            Err(e) => log::debug!("DCP: parse failed for {}: {e}", path.display()),
        }
    }
    Ok(None)
}

fn load_and_match(path: &Path, target: &str) -> Result<Option<Dcp>, String> {
    let bytes = fs::read(path).map_err(|e| format!("{e}"))?;
    let dcp = parse(&bytes).map_err(|e| format!("{e}"))?;
    if dcp_matches(&dcp, target) {
        Ok(Some(dcp))
    } else {
        Ok(None)
    }
}

/// True if the DCP's `UniqueCameraModel` (or, failing that, its
/// `ProfileCalibrationSignature`) matches the camera identity. Comparison is
/// case-insensitive and whitespace-insensitive, matching the DNG spec's
/// "loose match" rule.
fn dcp_matches(dcp: &Dcp, target: &str) -> bool {
    if let Some(ref m) = dcp.unique_camera_model
        && normalize(m) == target
    {
        return true;
    }
    if let Some(ref sig) = dcp.profile_calibration_signature
        && normalize(sig) == target
    {
        return true;
    }
    false
}

/// Normalize a camera identity string: lowercase + collapse whitespace.
fn normalize(s: &str) -> String {
    // Small alloc but we rarely hit this (once per decode + per file scan).
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c.to_ascii_lowercase());
            last_was_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Lazily log the outcome of "is ACR's directory present?" once per process
/// so the logs don't repeat on every file load.
pub fn log_search_summary_once() {
    static LOGGED: OnceLock<()> = OnceLock::new();
    LOGGED.get_or_init(|| {
        let dirs = search_dirs();
        let mut found_any = false;
        for dir in &dirs {
            if dir.exists() {
                log::debug!("DCP search path: {} (present)", dir.display());
                found_any = true;
            } else {
                log::debug!("DCP search path: {} (absent)", dir.display());
            }
        }
        if !found_any {
            log::info!(
                "DCP: no search directories present; set {} to enable per-camera profiles",
                DCP_DIR_ENV_VAR
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_whitespace_and_cases() {
        assert_eq!(normalize("  Sony   ILCE-7M3 "), "sony ilce-7m3");
        assert_eq!(normalize("Canon\tEOS\nR6"), "canon eos r6");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn empty_camera_id_returns_none() {
        assert!(find_dcp_for_camera("").is_none());
        assert!(find_dcp_for_camera("   ").is_none());
    }

    #[test]
    fn dcp_matches_by_unique_camera_model() {
        let dcp = Dcp {
            unique_camera_model: Some("Sony ILCE-7M3".to_string()),
            profile_name: None,
            profile_copyright: None,
            profile_calibration_signature: None,
            calibration_illuminant_1: None,
            calibration_illuminant_2: None,
            hue_sat_map_1: None,
            hue_sat_map_2: None,
            hue_sat_map_encoding: 0,
            look_table: None,
            look_table_encoding: 0,
            tone_curve: None,
        };
        assert!(dcp_matches(&dcp, &normalize("Sony ILCE-7M3")));
        assert!(dcp_matches(&dcp, &normalize("SONY    ilce-7m3")));
        assert!(!dcp_matches(&dcp, &normalize("Sony ILCE-7M4")));
    }

    #[test]
    fn dcp_matches_by_calibration_signature() {
        let dcp = Dcp {
            unique_camera_model: Some("Sony ILCE-7M3".to_string()),
            profile_name: None,
            profile_copyright: None,
            profile_calibration_signature: Some("custom-sig-xyz".to_string()),
            calibration_illuminant_1: None,
            calibration_illuminant_2: None,
            hue_sat_map_1: None,
            hue_sat_map_2: None,
            hue_sat_map_encoding: 0,
            look_table: None,
            look_table_encoding: 0,
            tone_curve: None,
        };
        assert!(dcp_matches(&dcp, &normalize("custom-sig-xyz")));
    }

    /// Smoke test for the full `find_dcp_for_camera` path: put a synthetic
    /// DCP in a temp dir, point `PRVW_DCP_DIR` at it, and confirm the
    /// discoverer finds it and returns the parsed struct.
    ///
    /// Gated to macOS because other tests in the color module already lean
    /// on macOS specifics; keeping this one consistent avoids surprise
    /// Linux CI failures from a serial env-var race against another suite.
    #[cfg(target_os = "macos")]
    #[test]
    fn find_dcp_uses_env_var_path() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let bytes = crate::color::dcp::tests::tiny_identity_dcp("Test Camera");
        std::fs::write(tmp.path().join("test.dcp"), &bytes).unwrap();
        // SAFETY: Each test lives in its own process in practice; we set
        // and clear the env var synchronously within the test body. There
        // is no other test in this module that reads `PRVW_DCP_DIR`.
        unsafe {
            std::env::set_var(DCP_DIR_ENV_VAR, tmp.path());
        }
        let dcp = find_dcp_for_camera("Test Camera").expect("should find DCP");
        assert_eq!(dcp.unique_camera_model.as_deref(), Some("Test Camera"));
        unsafe {
            std::env::remove_var(DCP_DIR_ENV_VAR);
        }
    }
}
