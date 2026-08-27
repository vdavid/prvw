//! The gate that keeps the shared suite honest: an app the shared tests can only get by naming
//! the actions they exercise, checked against the parity registries.
//!
//! The names are `parity::CommandKey` variants, resolved through `GET /parity` rather than by
//! linking the registry into the test binary. The app renders that table from the layer-1
//! registries themselves, so a shared test's idea of what a platform owes can't drift from what
//! the exhaustive matches say. A name that doesn't resolve fails the test on the spot, which is
//! why a typo can't quietly disable a gate.

use std::ops::Deref;

use super::app::TestApp;

/// The host's spelling in the parity table (`parity::Platform::name`).
pub const HOST_PLATFORM: &str = if cfg!(target_os = "macos") {
    "macOS"
} else if cfg!(target_os = "windows") {
    "Windows"
} else {
    "Linux"
};

/// A running app the shared suite is allowed to assert against, plus the capability check that
/// let it start. Derefs to [`TestApp`], so every endpoint helper reads the same.
pub struct SharedApp {
    app: TestApp,
}

impl SharedApp {
    /// Start on the default fixture, or return `None` when the host doesn't apply.
    ///
    /// `commands` names the `CommandKey` variants the test exercises. Pass `&[]` for a test that
    /// asserts about a subsystem no platform forks (the state contract itself, live folder sync,
    /// window geometry). An empty list is a claim that nothing here is forked, so it's worth a
    /// second look before writing one.
    pub fn start(commands: &[&str]) -> Option<Self> {
        if !display_available() {
            return None;
        }
        Self::gate(TestApp::start(), commands)
    }

    /// Start on a custom image, or return `None` when the host doesn't apply.
    pub fn start_with_image(commands: &[&str], image_path: &std::path::Path) -> Option<Self> {
        if !display_available() {
            return None;
        }
        Self::gate(TestApp::start_with_image(image_path), commands)
    }

    /// Start on several image files, in the order given, or return `None` when the host doesn't
    /// apply. The first one is what opens; the rest join the list it navigates.
    pub fn start_with_images(commands: &[&str], images: &[&std::path::Path]) -> Option<Self> {
        if !display_available() {
            return None;
        }
        Self::gate(TestApp::start_with_images(images), commands)
    }

    /// Start on a directory (a dir-arg launch, which boots into browse mode), with the home
    /// directory overridden so the tree's roots contain it and the reveal walk is short. Returns
    /// `None` when the host doesn't apply.
    pub fn start_browse_dir(
        commands: &[&str],
        dir: &std::path::Path,
        home: &std::path::Path,
    ) -> Option<Self> {
        if !display_available() {
            return None;
        }
        Self::gate(TestApp::start_browse_dir(dir, home), commands)
    }

    /// Start on one image with the home directory overridden, or `None` when the host doesn't
    /// apply. The home override is what keeps a browse reveal from that image short.
    pub fn start_with_image_and_home(
        commands: &[&str],
        image: &std::path::Path,
        home: &std::path::Path,
    ) -> Option<Self> {
        if !display_available() {
            return None;
        }
        Self::gate(
            TestApp::start_with_arg_and_home(image, Some(home)),
            commands,
        )
    }

    fn gate(app: TestApp, commands: &[&str]) -> Option<Self> {
        let table = app.get_parity();
        for command in commands {
            match host_coverage(&table, command) {
                Coverage::Present => {}
                Coverage::NotApplicable(reason) => {
                    eprintln!(
                        "SKIP {}: {command} is not applicable on {HOST_PLATFORM}. {reason}",
                        current_test_name()
                    );
                    return None;
                }
                Coverage::Missing => panic!(
                    "{command} is a shared behaviour {HOST_PLATFORM} hasn't built yet \
                     (the parity registry says `missing`). Build it, or say why it's \
                     `NotApplicable` in `src/parity/command_keys.rs`."
                ),
            }
        }
        Some(Self { app })
    }
}

impl Deref for SharedApp {
    type Target = TestApp;

    fn deref(&self) -> &TestApp {
        &self.app
    }
}

/// Whether this host can put a window on a screen at all.
///
/// The suite spawns the real binary, which builds a `winit` window and a `wgpu` surface, so a
/// display server is a precondition rather than a detail. macOS and Windows always have a
/// session behind a logged-in runner; a bare Linux CI container has neither `DISPLAY` nor
/// `WAYLAND_DISPLAY`, and every test would fail ten seconds into waiting for a QA server that
/// never came up. Skipping says so once per test instead.
///
/// This is also the switch that turns the suite on: point the Linux job at an `xvfb-run` (or a
/// real session) and `DISPLAY` appears, so the tests start running with no change here.
fn display_available() -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    let has_display = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    if !has_display {
        eprintln!(
            "SKIP {}: no display server, so the app can't open a window. \
             Run the suite under a session (`xvfb-run` will do).",
            current_test_name()
        );
    }
    has_display
}

/// What the parity table says the host does with one command.
#[derive(Debug, PartialEq, Eq)]
pub enum Coverage {
    Present,
    NotApplicable(String),
    Missing,
}

/// Look one command up in the served parity table and return the host's status.
///
/// Split out from [`SharedApp::gate`] so the skip and fail branches can be tested from a Mac,
/// where every command is `Present` and neither would otherwise ever run.
pub fn host_coverage(table: &serde_json::Value, command: &str) -> Coverage {
    let entries = table["entries"]
        .as_array()
        .expect("the parity table has an `entries` array");
    let entry = entries
        .iter()
        .find(|e| e["registry"].as_str() == Some("Command") && e["name"].as_str() == Some(command))
        .unwrap_or_else(|| {
            panic!(
                "no `CommandKey` called {command}. The shared suite names actions the way \
                 `src/parity/command_keys.rs` does."
            )
        });
    let platform = entry["platforms"]
        .as_array()
        .expect("an entry lists its platforms")
        .iter()
        .find(|p| p["platform"].as_str() == Some(HOST_PLATFORM))
        .unwrap_or_else(|| panic!("the parity table says nothing about {HOST_PLATFORM}"));
    match platform["status"].as_str() {
        Some("done") => Coverage::Present,
        Some("not applicable") => Coverage::NotApplicable(
            platform["reason"]
                .as_str()
                .unwrap_or("No reason given.")
                .to_string(),
        ),
        Some("missing") => Coverage::Missing,
        other => panic!("unknown parity status {other:?} for {command}"),
    }
}

/// The running test's name, for the skip line. libtest and nextest both name the thread after
/// the test, so this reads as the test's own report rather than an anonymous one.
fn current_test_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("a shared test")
        .to_string()
}

/// Reject the two ways a macOS-only assertion sneaks into the shared suite: a platform `cfg`,
/// and a raw [`TestApp`] that skips the capability gate. Called by the shared suite against its
/// own source, so the failure lands on a Mac rather than on the first Windows CI run.
///
/// The needles live here rather than in the file under test, because a scan for a string can't
/// live in the text it scans.
pub fn assert_source_is_platform_neutral(src: &str) {
    let forbidden = [
        (
            "target_os",
            "the shared suite runs on every platform, so it can't branch on one. \
             A behaviour that only exists on one platform belongs in that platform's driver, \
             or behind a `SharedApp` capability if the registry already knows about it.",
        ),
        (
            "target_family",
            "same as `target_os`: the shared suite doesn't branch on the host.",
        ),
        (
            "TestApp",
            "the shared suite goes through `SharedApp`, which makes it name the actions it \
             exercises. A raw `TestApp` skips the parity gate.",
        ),
    ];
    for (needle, why) in forbidden {
        assert!(
            !src.contains(needle),
            "`{needle}` appears in the shared E2E suite: {why}"
        );
    }
}
