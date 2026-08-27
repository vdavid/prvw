//! The E2E harness: spawn the real `prvw` binary and drive it through the QA HTTP server.
//!
//! This module compiles on every platform, and so does everything in it. It backs two test
//! targets, which is the whole point of layer 3 of the parity harness
//! (`docs/specs/cross-platform-plan.md`, M0.5):
//!
//! - `tests/e2e_shared.rs` — the platform-neutral core. Every assertion is about observable
//!   state through `/state`, `/key`, and the other QA endpoints, so it runs anywhere the app
//!   runs. Layer 1 proves a toggle exists on both platforms; this proves it does the same
//!   thing.
//! - `tests/e2e_macos.rs` — the macOS driver. Everything that has to poke a native widget:
//!   browse mode's `NSOutlineView` and `NSCollectionView`, the AppKit settings window, the
//!   AppKit fullscreen transition, and the `screencapture` MCP tool.
//!
//! ## What keeps the split honest
//!
//! [`shared::SharedApp`] is the only way the shared suite can get an app, and its constructors
//! take a list of [`parity::CommandKey`] names. The gate resolves them against `GET /parity`,
//! which the app renders from the layer-1 registries, so it can't drift from what the platforms
//! actually owe each other:
//!
//! - `done` on the host: the test runs.
//! - `not applicable`: the test skips, printing the registry's reason.
//! - `missing`: the test fails, naming the key. A shared behaviour a platform hasn't built yet
//!   is a gap in the app, and layer 3's job is to say so.
//!
//! `shared_suite_stays_platform_neutral` (in `e2e_shared.rs`) closes the back door by rejecting
//! `target_os` and bare `TestApp` in that file.
//!
//! Rust has no way for a test to report itself skipped, so a skipped one reports as passing and
//! writes a `SKIP <test>: <reason>` line to stderr. `cargo nextest run --no-capture` is how you
//! read those; `docs/parity.md` is how you predict them.
//!
//! ## Off-macOS caveats
//!
//! The shared suite compiles for Windows and Linux and has run on neither, so its value is
//! latent until the first CI run on one. These are the assumptions to check first when it
//! happens. `src/qa/CLAUDE.md` carries the same list.
//!
//! - **A window has to be able to open.** The harness spawns the real binary, which builds a
//!   `winit` window and a `wgpu` surface. [`shared::SharedApp`] skips the whole suite on a Linux
//!   host with no `DISPLAY` or `WAYLAND_DISPLAY`, because every test would otherwise fail ten
//!   seconds into waiting for a QA server that never started. Give the job a session and they
//!   run.
//! - **Timing is tuned to macOS.** The fixed 500 ms after startup, the 150 ms after a key press,
//!   and the live-sync timeouts were all measured against FSEvents on a Mac.
//!   `ReadDirectoryChangesW` and inotify have their own latency, and `TestApp::wait_for_watch`
//!   only covers the watch being armed, not how long an event then takes to arrive.
//! - **Paths.** Nothing here hardcodes a POSIX path (every fixture is a `tempfile` dir), and the
//!   file assertions match on the file name rather than the separator. What isn't covered is the
//!   app's own path handling, which is M1 step 10.
//! - **`HOME`, and what Windows has instead of it.** `TestApp` sets both `HOME` and
//!   `USERPROFILE`, but only macOS reads either: the Windows tree's roots come from
//!   `SHGetKnownFolderPath` and the drive letters, which no environment variable can scope. So a
//!   fixture there is revealed by walking down from `C:\`, through the hidden `AppData` its temp
//!   folder lives under — which is why `TreeScanner::scan_revealing` exists. The walk is longer
//!   and slower than the macOS one, and the browse timeouts have to hold for it.
//! - **A directory argument.** `main.rs` accepts one everywhere. macOS and Windows boot into
//!   browse mode; Linux, which has no browser, opens the folder's images in image mode starting
//!   at the first.
//!
//! ## What this gate can't ask for
//!
//! `SharedApp::start` names the actions a test needs, so it can say "this test needs browse
//! mode", never "this test needs the platforms that don't have browse mode". The behaviours
//! that exist *because* a platform is missing a feature therefore can't live here: the folder
//! argument's image-mode fallback, and the empty window a no-argument launch puts up
//! (`app::EmptyState::NothingOpen`, which macOS never reaches because it waits for Finder's
//! Apple Event). Those are unit-tested in `src/launch.rs` instead, for every platform at once.

// Each test target compiles the whole harness, so whatever one of them doesn't use looks dead
// there. The alternative is splitting the harness per target, which is what this module exists
// to avoid.
#![allow(dead_code)]

pub mod app;
pub mod fixtures;
pub mod shared;
