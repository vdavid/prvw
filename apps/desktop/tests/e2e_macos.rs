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
//! - **Finder tags** are a macOS filesystem fact: what a test has to check is the extended
//!   attribute Finder reads, and `ToggleTag` is `not applicable` everywhere else.
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

// ── Finder tags ──────────────────────────────────────────────────────────────────────────────

/// The attribute Finder keeps a file's tags in.
const TAGS_XATTR: &str = "com.apple.metadata:_kMDItemUserTags";

/// The raw tag attribute on `path`, or `None` when the file has none. Read with the system's own
/// `xattr` tool, so the test checks the file rather than the app's reading of it.
fn tag_attribute(path: &std::path::Path) -> Option<Vec<u8>> {
    let output = std::process::Command::new("/usr/bin/xattr")
        .args(["-px", TAGS_XATTR])
        .arg(path)
        .output()
        .expect("run xattr");
    if !output.status.success() {
        return None;
    }
    let hex: String = String::from_utf8(output.stdout)
        .expect("xattr prints hex")
        .split_whitespace()
        .collect();
    Some(
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex"))
            .collect(),
    )
}

/// Whether the attribute holds this `"Name\nN"` tag string. A binary plist stores short ASCII
/// strings inline, so the bytes are there to find.
fn carries(attribute: &[u8], tag: &str) -> bool {
    attribute
        .windows(tag.len())
        .any(|window| window == tag.as_bytes())
}

/// The tags `/state` reports, as `(name, color)` pairs.
fn state_tags(state: &serde_json::Value) -> Option<Vec<(String, Option<String>)>> {
    state["tags"].as_array().map(|tags| {
        tags.iter()
            .map(|tag| {
                (
                    tag["name"].as_str().unwrap_or_default().to_string(),
                    tag["color"].as_str().map(str::to_string),
                )
            })
            .collect()
    })
}

fn tags_of(pairs: &[(&str, Option<&str>)]) -> Option<Vec<(String, Option<String>)>> {
    Some(
        pairs
            .iter()
            .map(|(name, color)| (name.to_string(), color.map(str::to_string)))
            .collect(),
    )
}

#[test]
fn digit_keys_toggle_finder_tags_and_keep_the_others() {
    let dir = tempfile::tempdir().expect("temp dir");
    let image = dir.path().join("photo.png");
    e2e::fixtures::create_fixture_image(&image);
    // What Finder writes for a colorless "Important" plus its red tag.
    let seeded = std::process::Command::new("/usr/bin/xattr")
        .args([
            "-wx",
            TAGS_XATTR,
            "62706C6973743030A201025B496D706F7274616E740A30555265640A36080B17000000000000010100000000000000030000000000000000000000000000001D",
        ])
        .arg(&image)
        .status()
        .expect("run xattr");
    assert!(seeded.success());

    let app = TestApp::start_with_image(&image);
    let wait = Duration::from_secs(10);
    let read = app.wait_for_state(wait, |s| state_tags(s).is_some_and(|t| t.len() == 2));
    assert_eq!(
        state_tags(&read),
        tags_of(&[("Important", None), ("Red", Some("red"))])
    );

    // `1` is red, which the file has: it comes off, and the colorless tag stays.
    app.post("/key", "1");
    let removed = app.wait_for_state(wait, |s| state_tags(s).is_some_and(|t| t.len() == 1));
    assert_eq!(state_tags(&removed), tags_of(&[("Important", None)]));
    let on_disk = tag_attribute(&image).expect("the colorless tag keeps the attribute");
    assert!(carries(&on_disk, "Important\n0"));
    assert!(!carries(&on_disk, "Red\n6"));

    // `5` is blue, which it doesn't have: it goes on, after the tags already there.
    app.post("/key", "5");
    let added = app.wait_for_state(wait, |s| state_tags(s).is_some_and(|t| t.len() == 2));
    assert_eq!(
        state_tags(&added),
        tags_of(&[("Important", None), ("Blue", Some("blue"))])
    );
    let on_disk = tag_attribute(&image).expect("attribute");
    assert!(carries(&on_disk, "Important\n0"));
    assert!(carries(&on_disk, "Blue\n4"));
}

#[test]
fn toggling_the_last_tag_off_removes_the_attribute() {
    let dir = tempfile::tempdir().expect("temp dir");
    let image = dir.path().join("photo.png");
    e2e::fixtures::create_fixture_image(&image);

    let app = TestApp::start_with_image(&image);
    let wait = Duration::from_secs(10);
    let untagged = app.wait_for_state(wait, |s| state_tags(s).is_some());
    assert_eq!(state_tags(&untagged), tags_of(&[]));
    assert!(tag_attribute(&image).is_none());

    app.post("/key", "3");
    let yellow = app.wait_for_state(wait, |s| state_tags(s).is_some_and(|t| !t.is_empty()));
    assert_eq!(state_tags(&yellow), tags_of(&[("Yellow", Some("yellow"))]));
    assert!(carries(
        &tag_attribute(&image).expect("attribute"),
        "Yellow\n5"
    ));

    app.post("/key", "3");
    let cleared = app.wait_for_state(wait, |s| state_tags(s).is_some_and(|t| t.is_empty()));
    assert_eq!(state_tags(&cleared), tags_of(&[]));
    assert!(
        tag_attribute(&image).is_none(),
        "clearing the last tag removes the attribute, the way Finder does"
    );
}

/// Browse mode shows no single image, so there's nothing to tag: `/state` says so, and a digit
/// that reaches the app leaves the file alone.
#[test]
fn browse_mode_has_no_image_to_tag() {
    let app = TestApp::start();
    app.wait_for_state(Duration::from_secs(10), |s| state_tags(s).is_some());
    let image = std::path::PathBuf::from(app.get_state()["file"].as_str().expect("a file"));
    app.post("/key", "Enter");
    let browsing = app.wait_for_state(Duration::from_secs(10), |s| {
        s["view_mode"].as_str() == Some("browse")
    });
    assert!(browsing["tags"].is_null(), "{}", browsing["tags"]);
    app.post("/key", "1");
    assert!(tag_attribute(&image).is_none());
}
