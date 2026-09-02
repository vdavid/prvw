//! # Update check
//!
//! Whether a newer Prvw exists, and what the platform does about it. Governed by
//! `Settings::auto_update`, which the General panel calls "Check for updates when Prvw
//! starts".
//!
//! The decision is one thing and the action is another, so they're separate modules:
//!
//! - [`manifest`] fetches nothing. It holds the shape of `latest.json`, the key each build
//!   looks for in it, the semver comparison, the once-per-release rule, and the once-per-launch
//!   one. Pure, compiled on every platform, and unit-tested from any host.
//! - [`check`] does the network round trip and hands the answer to the platform.
//! - [`macos`] downloads the DMG and swaps the running bundle in place.
//! - [`windows`] opens the download in the browser, because the app ships as an installer
//!   there and only a person can click through one.
//!
//! Linux compiles the policy and calls none of it: Prvw doesn't publish Linux builds, so
//! there's nothing to check against and nowhere to send anyone.
//!
//! Both entry points check `https://getprvw.com/latest.json` (override with
//! `PRVW_UPDATE_URL`), and only a copy that was installed runs at all: see `check::should_check`.
//!
//! **One fetch per launch.** A Finder double-click runs both entry points: [`check::check_only`]
//! while it waits for the Apple Event, then [`check::check_on_launch`] once the file lands. The
//! second reads what the first found, through `manifest::UpdateCheck`, rather than asking the host
//! again — two round trips for one answer, both competing with the read of the image the user is
//! actually waiting for. A launch that names a file skips the first entry point and the second
//! does the single fetch itself.

mod manifest;

// The half that touches the network and the machine.
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod check;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use check::check_on_launch;
#[cfg(target_os = "macos")]
pub use check::check_only;
