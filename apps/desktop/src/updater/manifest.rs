//! What a check for updates decides, with nothing in it that touches the machine.
//!
//! `https://getprvw.com/latest.json` is one document for every platform: a version, and a
//! `platforms` map from a key like `darwin-aarch64` to the file that build should download.
//! Which key a build looks for, whether the published version is newer than the running one,
//! and whether the user has already been shown it are all pure decisions, so they live here
//! where any host can assert every platform's answer. [`super::macos`] and [`super::windows`]
//! hold the halves that download, swap a bundle, or open a browser.

use serde::Deserialize;
use std::collections::HashMap;

/// The manifest the release workflow publishes. Unknown fields (`pub_date`, `dmgSizes`) are
/// ignored, so a manifest can grow without stranding the builds already out there.
#[derive(Debug, Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    /// Absent for a release that hasn't published a file for anyone yet. `default` so that
    /// reads as an empty map rather than a parse error, which would look identical to the
    /// host being down.
    #[serde(default)]
    pub platforms: HashMap<String, PlatformEntry>,
}

#[derive(Debug, Deserialize)]
pub struct PlatformEntry {
    pub url: String,
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing newer is published, or the manifest names a version we can't read.
    UpToDate,
    /// A newer release exists. `url` is the file this build should fetch, and it's `None`
    /// when the manifest carries no entry for this platform key: a release that hasn't
    /// published for this OS yet is still worth telling someone about, it just can't be
    /// handed to them directly.
    Available {
        version: String,
        url: Option<String>,
    },
}

/// The `platforms` key a build of `os` and `arch` looks for, from `std::env::consts`.
///
/// macOS spells itself `darwin` there, which is what the release workflow has always
/// published and what every shipped Mac build already asks for. Every other platform uses the
/// Rust name, so a 64-bit Windows build looks for `windows-x86_64`.
pub fn platform_key(os: &str, arch: &str) -> String {
    let os = if os == "macos" { "darwin" } else { os };
    format!("{os}-{arch}")
}

/// The key this build looks for.
pub fn host_platform_key() -> String {
    platform_key(std::env::consts::OS, std::env::consts::ARCH)
}

/// What the manifest means for a build running `current_version` on `platform_key`.
pub fn evaluate(manifest: &UpdateManifest, current_version: &str, platform_key: &str) -> Outcome {
    if !is_newer(&manifest.version, current_version) {
        return Outcome::UpToDate;
    }
    Outcome::Available {
        version: manifest.version.clone(),
        url: manifest
            .platforms
            .get(platform_key)
            .map(|entry| entry.url.clone()),
    }
}

/// Whether `remote` is a later release than `current`. Anything either side can't be read as
/// `major.minor.patch` answers `false`: a manifest we don't understand is never a reason to
/// act.
pub fn is_newer(remote: &str, current: &str) -> bool {
    match (parse_version(remote), parse_version(current)) {
        (Some(remote), Some(current)) => remote > current,
        _ => false,
    }
}

/// `major.minor.patch`, and nothing else. No `v` prefix, no pre-release suffix: the release
/// workflow writes plain numbers, so anything else in the field means we're reading a document
/// we don't understand.
fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

/// Whether to put a newer version in front of the user, given the one we last showed them.
///
/// Announcing a release once is the whole point: someone who reads the download page and
/// decides to stay where they are shouldn't meet it again on every launch.
///
/// Only Windows announces anything (macOS installs the update and says so afterwards), and
/// the rule is worth asserting from any host, so it lives here with no caller on a Mac.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn should_announce(available: &str, last_announced: Option<&str>) -> bool {
    last_announced != Some(available)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(json: &str) -> UpdateManifest {
        serde_json::from_str(json).expect("manifest parses")
    }

    #[test]
    fn a_later_release_is_newer() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("1.1.0", "1.0.9"));
        assert!(is_newer("2.0.0", "1.9.9"));
        assert!(is_newer("0.16.0", "0.15.1"));
    }

    #[test]
    fn the_same_release_is_not_newer() {
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.15.1", "0.15.1"));
    }

    #[test]
    fn an_earlier_release_is_not_newer() {
        assert!(!is_newer("0.9.0", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.1"));
    }

    #[test]
    fn a_version_we_cannot_read_is_never_newer() {
        assert!(!is_newer("invalid", "1.0.0"));
        assert!(!is_newer("1.0", "1.0.0"));
        assert!(!is_newer("1.0.0.1", "1.0.0"));
        assert!(!is_newer("v1.0.1", "1.0.0"));
        assert!(!is_newer("1.0.1", "not-a-version"));
    }

    #[test]
    fn macos_asks_for_darwin_and_everyone_else_for_their_own_name() {
        assert_eq!(platform_key("macos", "aarch64"), "darwin-aarch64");
        assert_eq!(platform_key("macos", "x86_64"), "darwin-x86_64");
        assert_eq!(platform_key("windows", "x86_64"), "windows-x86_64");
        assert_eq!(platform_key("windows", "aarch64"), "windows-aarch64");
        assert_eq!(platform_key("linux", "x86_64"), "linux-x86_64");
    }

    #[test]
    fn the_host_key_is_one_of_the_keys_we_publish() {
        let key = host_platform_key();
        assert!(key.contains('-'), "{key} should be os-arch");
        assert!(!key.starts_with("macos-"), "macOS publishes as darwin");
    }

    #[test]
    fn a_newer_manifest_hands_back_this_platforms_file() {
        let m = manifest(
            r#"{"version":"0.16.0","platforms":{
                "darwin-aarch64":{"url":"https://example.test/mac.dmg"},
                "windows-x86_64":{"url":"https://example.test/setup.exe"}}}"#,
        );
        assert_eq!(
            evaluate(&m, "0.15.1", "windows-x86_64"),
            Outcome::Available {
                version: "0.16.0".into(),
                url: Some("https://example.test/setup.exe".into()),
            }
        );
    }

    #[test]
    fn the_current_version_is_up_to_date() {
        let m = manifest(r#"{"version":"0.15.1","platforms":{"windows-x86_64":{"url":"u"}}}"#);
        assert_eq!(evaluate(&m, "0.15.1", "windows-x86_64"), Outcome::UpToDate);
    }

    #[test]
    fn an_older_manifest_is_up_to_date() {
        let m = manifest(r#"{"version":"0.14.0","platforms":{"windows-x86_64":{"url":"u"}}}"#);
        assert_eq!(evaluate(&m, "0.15.1", "windows-x86_64"), Outcome::UpToDate);
    }

    #[test]
    fn a_version_we_cannot_read_leaves_us_up_to_date() {
        let m = manifest(r#"{"version":"nightly","platforms":{"windows-x86_64":{"url":"u"}}}"#);
        assert_eq!(evaluate(&m, "0.15.1", "windows-x86_64"), Outcome::UpToDate);
    }

    /// The Mac-only manifest that's published today, read by a Windows build. It's newer, and
    /// there's nothing for this platform to download yet, which is a fact to report rather
    /// than an error.
    #[test]
    fn a_missing_platform_key_is_still_an_available_update() {
        let m = manifest(
            r#"{"version":"0.16.0","platforms":{"darwin-aarch64":{"url":"https://x/mac.dmg"}}}"#,
        );
        assert_eq!(
            evaluate(&m, "0.15.1", "windows-x86_64"),
            Outcome::Available {
                version: "0.16.0".into(),
                url: None,
            }
        );
    }

    #[test]
    fn a_manifest_with_no_platforms_at_all_still_parses() {
        let m = manifest(r#"{"version":"0.16.0"}"#);
        assert!(m.platforms.is_empty());
        assert_eq!(
            evaluate(&m, "0.15.1", "windows-x86_64"),
            Outcome::Available {
                version: "0.16.0".into(),
                url: None,
            }
        );
    }

    #[test]
    fn a_malformed_manifest_fails_to_parse_rather_than_panicking() {
        assert!(serde_json::from_str::<UpdateManifest>("not json at all").is_err());
        assert!(serde_json::from_str::<UpdateManifest>(r#"{"platforms":{}}"#).is_err());
        assert!(serde_json::from_str::<UpdateManifest>(r#"{"version":7}"#).is_err());
        assert!(
            serde_json::from_str::<UpdateManifest>(r#"{"version":"1.0.0","platforms":[]}"#)
                .is_err()
        );
    }

    #[test]
    fn the_live_manifest_shape_still_reads() {
        let m = manifest(include_str!("../../../website/public/latest.json"));
        assert!(!m.version.is_empty());
        assert!(m.platforms.contains_key("darwin-aarch64"));
    }

    #[test]
    fn a_version_we_have_never_shown_is_announced() {
        assert!(should_announce("0.16.0", None));
        assert!(should_announce("0.16.0", Some("0.15.1")));
    }

    #[test]
    fn the_version_we_last_showed_is_not_announced_again() {
        assert!(!should_announce("0.16.0", Some("0.16.0")));
    }
}
