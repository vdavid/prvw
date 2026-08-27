//! The macOS E2E driver: the parts of the app that can only be asserted by poking AppKit.
//!
//! Layer 3 of the parity harness (`docs/specs/cross-platform-plan.md`, M0.5) splits the suite in
//! two. `tests/e2e_shared.rs` holds everything that asserts about observable state and runs on
//! every platform; this file holds the rest. A test belongs here when the behaviour it asserts
//! is a macOS window-system fact, or when it drives a surface no other platform has:
//!
//! - **Browse mode's arrow keys** are a macOS fact: winit keeps the keyboard there, so the arrow
//!   set has to be driven through the QA server rather than by the focused pane. Windows gives
//!   its panes the focus and its own controls handle the arrows, so there is nothing to drive.
//!   The rest of browse mode is in the shared suite now, gated on `BrowseMode`, `BrowseFocus`,
//!   and `BrowseOpenSelected`.
//! - **The settings window** is an AppKit form (`src/settings/window.rs`); `Settings` is
//!   `missing` elsewhere.
//! - **The fullscreen round trip** is about AppKit's `toggleFullScreen:` and `winit`'s stale
//!   cache. Plain fullscreen behaviour is shared; this specific failure mode isn't.
//! - **`screenshot_window`** exists on Windows too, but only the macOS path needs a granted
//!   Screen Recording permission, which is what keeps this test `#[ignore]`d.
//!
//! When Windows grows any of these, its own driver file is the place for the equivalent, and the
//! parts that turn out to be genuinely the same move to the shared suite.

#![cfg(target_os = "macos")]

mod e2e;

use std::time::Duration;

use e2e::app::TestApp;

// The settings window's own tests live in `e2e_shared.rs`: both platforms build one now, and
// everything worth asserting about it (that it opens, that it switches sections, and that it
// leaves the app running) is observable through the QA server rather than through a widget.

// ── Fullscreen, the way AppKit drives it ─────────────────────────────────────────────────────

#[test]
fn fullscreen_state_survives_a_round_trip_appkit_drove() {
    // Fullscreen transitions go through AppKit (`toggleFullScreen:`), not `winit`, because the
    // green traffic light can start one too and `winit` never un-remembers those: its cached
    // state reads "fullscreen" for a restored window. Reading that cache dressed the restored
    // window as fullscreen — no title-bar strip, black background, two mismatched corner radii
    // — and inverted the next F. So the state has to come from AppKit, and it has to stay right
    // across a round trip and keep the keyboard toggle working.
    //
    // The green button itself can't drive this test: `/click-zoom-button` zooms rather than
    // going fullscreen for the harness's background, non-key window. The transition below takes
    // the same AppKit path the button takes.
    // Fullscreen transitions build and tear down a Space, which takes a moment even with the
    // `fullscreen` nextest group keeping them from overlapping.
    const FULLSCREEN_WAIT: Duration = Duration::from_secs(30);

    let app = TestApp::start();
    let windowed_width = app.get_state()["window_width"].as_u64().unwrap();
    assert_eq!(app.get_state()["fullscreen"].as_bool(), Some(false));

    // Wait for the window to actually fill the screen, not just for the state to flip: the
    // style mask carries the fullscreen bit from the moment the transition *starts*, and
    // AppKit drops a request to leave that arrives while it's still animating (`winit` queues
    // one, and only replays it once the entry finishes).
    app.post("/fullscreen", "on");
    let entered = app.wait_for_state(FULLSCREEN_WAIT, |s| {
        s["fullscreen"].as_bool() == Some(true)
            && s["window_width"]
                .as_u64()
                .is_some_and(|w| w > windowed_width)
    });
    assert_eq!(entered["fullscreen"].as_bool(), Some(true));
    assert!(entered["window_width"].as_u64().unwrap() > windowed_width);

    app.post("/fullscreen", "off");
    let left = app.wait_for_state(FULLSCREEN_WAIT, |s| {
        s["fullscreen"].as_bool() == Some(false)
    });
    assert_eq!(
        left["fullscreen"].as_bool(),
        Some(false),
        "the window must not be left believing it's fullscreen after leaving"
    );

    app.post("/key", "f");
    let toggled = app.wait_for_state(FULLSCREEN_WAIT, |s| {
        s["fullscreen"].as_bool() == Some(true)
            && s["window_width"]
                .as_u64()
                .is_some_and(|w| w > windowed_width)
    });
    assert_eq!(
        toggled["fullscreen"].as_bool(),
        Some(true),
        "F must still enter fullscreen after an AppKit-driven round trip"
    );
    app.post("/key", "f");
    app.wait_for_state(FULLSCREEN_WAIT, |s| {
        s["fullscreen"].as_bool() == Some(false)
    });
}

// ── The native window capture ────────────────────────────────────────────────────────────────

/// `screenshot_window` MCP tool runs end-to-end. Marked `#[ignore]` because the tool
/// shells out to `/usr/sbin/screencapture -l`, which requires Screen Recording
/// permission. Headless CI hosts and freshly-cloned dev boxes return a black
/// (still valid PNG) frame until the user grants it. Run locally with:
/// `cargo test --test e2e_macos screenshot_window_returns_png -- --ignored`.
#[test]
#[ignore]
fn screenshot_window_returns_png() {
    let app = TestApp::start();
    let result = app.mcp_call("screenshot_window", serde_json::json!({}));
    let content = result["result"]["content"]
        .as_array()
        .expect("screenshot_window should return a content array");
    let first = &content[0];
    assert_eq!(first["type"].as_str(), Some("image"));
    assert_eq!(first["mimeType"].as_str(), Some("image/png"));
    let b64 = first["data"]
        .as_str()
        .expect("data should be a base64 string");
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("data should be valid base64");
    assert!(!bytes.is_empty(), "PNG bytes should be non-empty");
    // Decode as PNG to confirm it's a real image, not just bytes.
    let img = image::load_from_memory(&bytes).expect("should decode as PNG");
    assert!(img.width() > 0 && img.height() > 0);
}

// ── Browse mode: the one part that is a macOS window-system fact ─────────────────────────────
//
// Everything else about browse mode moved to the shared suite when Windows grew a browser: the
// mode switch, the folder flow, and live sync are all observable through `/state` and are gated
// on `BrowseMode` / `BrowseFocus` / `BrowseOpenSelected`.

#[test]
fn browse_arrow_keys_drive_tree_without_crashing() {
    // The tree is driven programmatically (winit keeps the keyboard). Exercise the
    // full arrow set and confirm the app stays alive and in browse mode — the real
    // selection movement is a visual check, but this guards the command path.
    let app = TestApp::start();
    app.post("/key", "Enter");
    for key in [
        "ArrowDown",
        "ArrowDown",
        "ArrowRight",
        "ArrowDown",
        "ArrowUp",
        "ArrowLeft",
    ] {
        app.post("/key", key);
    }
    let state = app.get_state();
    assert_eq!(state["view_mode"].as_str(), Some("browse"));
    // The selected-folder field is present in the state contract (null until a row
    // is selected, a string path once the selection delegate fires).
    assert!(state.get("browse_selected_folder").is_some());
}
