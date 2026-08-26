//! The macOS E2E driver: the parts of the app that can only be asserted by poking AppKit.
//!
//! Layer 3 of the parity harness (`docs/specs/cross-platform-plan.md`, M0.5) splits the suite in
//! two. `tests/e2e_shared.rs` holds everything that asserts about observable state and runs on
//! every platform; this file holds the rest. A test belongs here when the behaviour it asserts
//! is a macOS window-system fact, or when it drives a surface no other platform has:
//!
//! - **Browse mode** is a native `NSOutlineView` + `NSCollectionView` (`src/browser/`). The
//!   parity registry has `BrowseMode`, `BrowseFocus`, and `BrowseOpenSelected` as `missing`
//!   everywhere else, and the QA driving hooks (`/browse/select-folder`, `/browse/select-grid`,
//!   `/browse/open`) answer 400 off macOS.
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
use e2e::fixtures::write_png;

// ── Settings window ──────────────────────────────────────────────────────────────────────────

#[test]
fn settings_opens_and_closes() {
    let app = TestApp::start();
    app.post("/show-settings", "");
    std::thread::sleep(Duration::from_millis(200));
    // Settings window is non-modal, app should still respond
    let state = app.get_state();
    assert!(
        state["file"].as_str().is_some(),
        "app should still be responsive with settings open"
    );
    app.post("/close-settings", "");
}

#[test]
fn settings_section_switch() {
    let app = TestApp::start();
    app.post("/show-settings", "file_associations");
    std::thread::sleep(Duration::from_millis(200));
    // Verify the app doesn't crash
    let state = app.get_state();
    assert!(state["file"].as_str().is_some());
    app.post("/show-settings", "general");
    std::thread::sleep(Duration::from_millis(200));
    app.post("/close-settings", "");
}

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

// ── Browse mode: the mode switch ─────────────────────────────────────────────────────────────

#[test]
fn enter_browse_mode_with_enter_key() {
    let app = TestApp::start();
    assert_eq!(app.get_state()["view_mode"].as_str(), Some("image"));
    // Enter swaps the image viewer for the native browse screen (tree + grid).
    app.post("/key", "Enter");
    let state = app.get_state();
    assert_eq!(state["view_mode"].as_str(), Some("browse"));
    // Browse starts focused on the tree pane.
    assert_eq!(state["focused_pane"].as_str(), Some("tree"));
}

#[test]
fn escape_returns_from_browse_to_image() {
    let app = TestApp::start();
    app.post("/key", "Enter");
    assert_eq!(app.get_state()["view_mode"].as_str(), Some("browse"));
    // Esc in browse mode returns to the image viewer (never quits the app).
    app.post("/key", "Escape");
    assert_eq!(app.get_state()["view_mode"].as_str(), Some("image"));
}

#[test]
fn enter_in_browse_returns_to_image_mode() {
    // Enter maps to "open the selected grid image". Entering browse from an image reveals that
    // image's folder, so Enter takes one of two routes depending on whether the reveal's listing
    // has landed yet: the grid opens the selected image, or the tree-focused fallback returns to
    // the current image. Both must land back in image mode, never a stray open and never stuck in
    // browse. Which route ran is a race, so this asserts only what holds either way; the routes
    // themselves are covered by `entering_browse_from_an_image_preselects_that_image` and
    // `empty_folder_lists_zero_and_grid_stays_non_focusable`.
    let app = TestApp::start();
    app.post("/key", "Enter"); // image → browse
    assert_eq!(app.get_state()["view_mode"].as_str(), Some("browse"));
    app.post("/key", "Enter"); // → back to image mode
    let state = app.get_state();
    assert_eq!(state["view_mode"].as_str(), Some("image"));
    // The grid-selection field is part of the state contract (null until the grid has a selection).
    assert!(state.get("browse_grid_selected").is_some());
    assert!(
        state["file"].as_str().unwrap().contains("fixture.png"),
        "Enter must land on the image we came from, got {state}"
    );
}

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

// ── Browse mode: the full flow, driven through the QA server ─────────────────────────────────
//
// These drive the full browse picture headlessly via the QA browse hooks (`/browse/select-folder`,
// `/browse/select-grid`, `/browse/open`) and assert the `/state` fields (`browse_grid_count`,
// `browse_reveal_pending`, `browse_selected_folder`, `browse_grid_selected`). They stay hermetic:
// each builds its own temp home so the tree's home root contains the test folders, and polls
// `/state` with a bounded wait (folder listing + tree reveal are async) rather than sleeping a
// fixed time.

/// Build a temp home with a subfolder of `n` distinct PNGs and an empty subfolder. Returns
/// `(home_tempdir, images_folder, empty_folder)`. The folders sit directly under home so a reveal
/// walk is a short, deterministic chain.
fn create_browse_home(n: u32) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let images = home.path().join("pics");
    let empty = home.path().join("empty");
    std::fs::create_dir(&images).unwrap();
    std::fs::create_dir(&empty).unwrap();
    for i in 0..n {
        let path = images.join(format!("img-{i:02}.png"));
        let shade = (i as u8).wrapping_mul(23);
        let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([shade, shade, shade, 255]));
        img.save(&path).unwrap();
    }
    (home, images, empty)
}

/// Wait until the browse reveal walk settles (`browse_reveal_pending == false`) AND the grid has
/// listed `expected_count` images. Returns the settled state. The reveal walk and the folder
/// listing are both async, so this is the non-flaky barrier the browse tests gate on.
fn wait_for_browse_listed(
    app: &TestApp,
    expected_count: u64,
    timeout: Duration,
) -> serde_json::Value {
    app.wait_for_state(timeout, |s| {
        // `view_mode` and `focused_pane` are part of the barrier, not decoration: before browse
        // mode is up, `browse_reveal_pending` is already `false` and `browse_grid_count` is already
        // `0`, so an `expected_count` of 0 would otherwise match the state the app starts in and
        // the caller would assert against a picture that hasn't happened yet.
        s["view_mode"].as_str() == Some("browse")
            && s["focused_pane"].as_str() != Some("none")
            && s["browse_reveal_pending"].as_bool() == Some(false)
            && s["browse_grid_count"].as_u64() == Some(expected_count)
    })
}

#[test]
fn dir_arg_launch_boots_into_browse_with_folder_revealed() {
    // A directory argument boots into browse mode with that directory revealed + selected in the
    // tree and its images listed in the grid.
    let (home, images, _empty) = create_browse_home(4);
    let app = TestApp::start_browse_dir(&images, home.path());

    let state = wait_for_browse_listed(&app, 4, Duration::from_secs(8));
    assert_eq!(
        state["view_mode"].as_str(),
        Some("browse"),
        "a directory argument boots into browse mode"
    );
    assert_eq!(
        state["browse_grid_count"].as_u64(),
        Some(4),
        "the revealed folder's four images are listed"
    );
    let selected = state["browse_selected_folder"]
        .as_str()
        .expect("the revealed folder is selected in the tree");
    assert!(
        selected.ends_with("pics"),
        "the selected folder is the launch directory, got {selected}"
    );
    // The grid preselects the first image (dir-arg launch has no came-from image).
    assert_eq!(state["browse_grid_selected"].as_u64(), Some(0));
}

#[test]
fn selecting_a_folder_lists_its_images() {
    // Selecting a folder in the tree (driven by path) lists its images: the grid count reflects the
    // folder's supported-image count.
    let (home, images, empty) = create_browse_home(3);
    let app = TestApp::start_browse_dir(&empty, home.path());

    // Launched into the empty folder → zero images, tree focused (grid non-focusable).
    let state = wait_for_browse_listed(&app, 0, Duration::from_secs(8));
    assert_eq!(state["browse_grid_count"].as_u64(), Some(0));
    assert_eq!(state["focused_pane"].as_str(), Some("tree"));

    // Select the images folder by path → it lists three images.
    app.post("/browse/select-folder", images.to_str().unwrap());
    let state = app.wait_for_state(Duration::from_secs(5), |s| {
        s["browse_grid_count"].as_u64() == Some(3)
    });
    assert_eq!(
        state["browse_grid_count"].as_u64(),
        Some(3),
        "selecting the images folder lists its three images"
    );
    let selected = state["browse_selected_folder"].as_str().unwrap();
    assert!(selected.ends_with("pics"), "got {selected}");
}

#[test]
fn dropping_a_folder_browses_it() {
    // A folder dropped on the window opens browse mode at that folder, the same answer a folder
    // on the command line gets. The shared suite only checks that browse mode came up, because
    // where the tree landed needs a scoped home to keep the reveal walk short and deterministic
    // (`TestApp::start_with_arg_and_home`); this is the half that needs one.
    let (home, images, _empty) = create_browse_home(4);
    let launch_image = images.join("img-00.png");
    let app = TestApp::start_with_arg_and_home(&launch_image, Some(home.path()));
    assert_eq!(
        app.get_state()["view_mode"].as_str(),
        Some("image"),
        "an image argument starts in image mode"
    );

    app.post("/drop", images.to_str().unwrap());

    let state = wait_for_browse_listed(&app, 4, Duration::from_secs(8));
    let selected = state["browse_selected_folder"]
        .as_str()
        .expect("the dropped folder is selected in the tree");
    assert!(
        selected.ends_with("pics"),
        "the selected folder is the one dropped, got {selected}"
    );
    assert_eq!(
        state["browse_grid_count"].as_u64(),
        Some(4),
        "the dropped folder's four images are listed"
    );
}

#[test]
fn empty_folder_lists_zero_and_grid_stays_non_focusable() {
    // An empty folder → zero images, "(No images)", grid non-focusable: Tab stays on the tree.
    // The dir-arg launch is what makes the empty grid deterministic: entering browse from an
    // image always reveals a folder that has at least that image in it.
    let (home, _images, empty) = create_browse_home(2);
    let app = TestApp::start_browse_dir(&empty, home.path());

    let state = wait_for_browse_listed(&app, 0, Duration::from_secs(8));
    assert_eq!(state["browse_grid_count"].as_u64(), Some(0));
    assert_eq!(state["browse_grid_selected"].as_u64(), None);
    assert_eq!(
        state["focused_pane"].as_str(),
        Some("tree"),
        "an empty folder leaves focus on the tree"
    );

    // Tab toward the empty grid stays on the tree (the grid can't take focus). `SendKey` runs
    // the mapped command inline (`app/executor.rs`) and `/key` answers only after the event
    // loop's sync barrier, so its reply is already the post-Tab state — no sleep needed, and a
    // sleep here would hide a regression that made Tab asynchronous rather than catch it.
    let after_tab = app.post("/key", "Tab");
    assert_eq!(
        after_tab["focused_pane"].as_str(),
        Some("tree"),
        "Tab on an empty grid stays on the tree"
    );
}

#[test]
fn tab_flips_focus_to_grid_when_it_has_images() {
    // With a non-empty grid, Tab flips focus tree ⇄ grid, reflected in `focused_pane`.
    let (home, images, _empty) = create_browse_home(3);
    let app = TestApp::start_browse_dir(&images, home.path());

    // Dir-arg launch into a non-empty folder focuses the grid once images land.
    let state = wait_for_browse_listed(&app, 3, Duration::from_secs(8));
    assert_eq!(
        state["focused_pane"].as_str(),
        Some("grid"),
        "launching into a non-empty folder focuses the grid"
    );

    // Tab → tree.
    app.post("/key", "Tab");
    let state = app.wait_for_state(Duration::from_secs(2), |s| {
        s["focused_pane"].as_str() == Some("tree")
    });
    assert_eq!(state["focused_pane"].as_str(), Some("tree"));

    // Tab → back to grid.
    app.post("/key", "Tab");
    let state = app.wait_for_state(Duration::from_secs(2), |s| {
        s["focused_pane"].as_str() == Some("grid")
    });
    assert_eq!(state["focused_pane"].as_str(), Some("grid"));
}

#[test]
fn grid_selection_drives_open_round_trip() {
    // The grid selection drives the image-mode current: selecting a grid index then opening
    // (Esc == Enter == reveal) lands image mode on that exact image. Round-trip: re-entering browse
    // preselects the same image.
    let (home, images, _empty) = create_browse_home(5);
    let app = TestApp::start_browse_dir(&images, home.path());
    wait_for_browse_listed(&app, 5, Duration::from_secs(8));

    // Select grid index 3 (the way a native click would).
    app.post("/browse/select-grid", "3");
    let state = app.wait_for_state(Duration::from_secs(2), |s| {
        s["browse_grid_selected"].as_u64() == Some(3)
    });
    assert_eq!(state["browse_grid_selected"].as_u64(), Some(3));

    // Open the selection → image mode, showing that image (1-based index 4 of 5).
    app.post("/browse/open", "");
    let state = app.wait_for_state(Duration::from_secs(5), |s| {
        s["view_mode"].as_str() == Some("image") && s["index"].as_u64() == Some(4)
    });
    assert_eq!(state["view_mode"].as_str(), Some("image"));
    assert_eq!(
        state["index"].as_u64(),
        Some(4),
        "open lands image mode on the grid-selected image (index 3 → 1-based 4)"
    );
    assert_eq!(state["total_files"].as_u64(), Some(5));
    let file = state["file"].as_str().unwrap();
    assert!(
        file.ends_with("img-03.png"),
        "open shows the selected image, got {file}"
    );

    // Round-trip: re-enter browse from this image — the grid preselects the same image.
    app.post("/key", "Enter");
    let state = wait_for_browse_listed(&app, 5, Duration::from_secs(5));
    assert_eq!(state["view_mode"].as_str(), Some("browse"));
    assert_eq!(
        state["browse_grid_selected"].as_u64(),
        Some(3),
        "re-entering browse from an image preselects that image in the grid"
    );
}

/// Enter in browse means "open the selected grid image", but the tree can hold focus — Tab puts
/// it there, and a folder with no images leaves it there. With no grid selection to open, Enter
/// has to fall back to returning to image mode on the image we came from: never a stray open,
/// never stuck in browse.
///
/// `enter_in_browse_returns_to_image_mode` can't pin this branch, because which route Enter takes
/// there depends on whether the reveal's listing has landed. So wait for the listing (which moves
/// focus to the grid), then Tab back to the tree, and the fallback is the only route left.
#[test]
fn tree_focused_enter_returns_to_the_image_we_came_from() {
    let (home, images, _empty) = create_browse_home(5);
    let first = images.join("img-00.png");
    let app = TestApp::start_with_arg_and_home(&first, Some(home.path()));

    app.post("/key", "Enter"); // image → browse
    let state = wait_for_browse_listed(&app, 5, Duration::from_secs(8));
    assert_eq!(
        state["focused_pane"].as_str(),
        Some("grid"),
        "the reveal lands focus on the grid, which is what we Tab away from"
    );

    let state = app.post("/key", "Tab");
    assert_eq!(
        state["focused_pane"].as_str(),
        Some("tree"),
        "Tab off a non-empty grid moves focus to the tree"
    );
    assert_eq!(
        state["view_mode"].as_str(),
        Some("browse"),
        "Tab must not leave browse mode"
    );

    let state = app.post("/key", "Enter");
    assert_eq!(
        state["view_mode"].as_str(),
        Some("image"),
        "tree-focused Enter leaves browse, got {state}"
    );
    assert!(
        state["file"].as_str().unwrap().ends_with("img-00.png"),
        "and lands on the image we came from, not a stray open: {state}"
    );
}

#[test]
fn entering_browse_from_an_image_preselects_that_image() {
    // Entering browse from a multi-image folder (in image mode) reveals that folder and preselects
    // the displayed image — even when it's not the first image. The home dir is scoped to the
    // folder's parent so the reveal walk is a short, deterministic chain.
    let (home, images, _empty) = create_browse_home(5);
    let first = images.join("img-00.png");
    let app = TestApp::start_with_arg_and_home(&first, Some(home.path()));

    // Navigate to the third image (1-based index 3) in image mode.
    for _ in 0..2 {
        app.post("/navigate", "next");
    }
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(3));
    assert_eq!(state["index"].as_u64(), Some(3));

    // Enter browse — the grid reveals the folder and preselects the came-from image (index 2).
    app.post("/key", "Enter");
    let state = wait_for_browse_listed(&app, 5, Duration::from_secs(8));
    assert_eq!(state["view_mode"].as_str(), Some("browse"));
    assert_eq!(
        state["browse_grid_selected"].as_u64(),
        Some(2),
        "entering browse preselects the displayed image (1-based 3 → grid index 2)"
    );
    assert_eq!(
        state["focused_pane"].as_str(),
        Some("grid"),
        "browse-open focuses the grid when the folder has images"
    );
}

// ── Live folder sync (browse mode: grid + tree) ──────────────────────────────────────────────
//
// These boot into browse mode on a temp folder (dir-arg launch) and mutate the listed folder from
// the test process, then poll `/state`'s browse fields for the grid to update live. The grid runs
// off the same watcher + off-thread re-scan as image mode (whose behaviour the shared suite
// covers); here the watch follows the grid's listed folder, so `wait_for_watch` takes the listed
// folder rather than the image's. The tree-structure reload (subfolder add/remove on an expanded
// node) isn't observable through `/state` (no tree-children field), so it's covered by logs + live
// QA, not here.

#[test]
fn live_sync_browse_grid_grows_when_an_image_is_added() {
    // Browse the `pics` folder (4 images), then drop a 5th into it from the shell side. The grid's
    // active-folder watch should pick it up and grow the listed count to 5.
    let (home, images, _empty) = create_browse_home(4);
    let app = TestApp::start_browse_dir(&images, home.path());
    wait_for_browse_listed(&app, 4, Duration::from_secs(8));
    app.wait_for_watch(&images);

    write_png(&images.join("img-99.png"), 13);

    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["browse_grid_count"].as_u64() == Some(5)
    });
    assert_eq!(
        state["browse_grid_count"].as_u64(),
        Some(5),
        "an image added to the listed folder should appear in the grid, got {state}"
    );
    // The selection (img-00, index 0) is unchanged — the add sorts after it.
    assert_eq!(state["browse_grid_selected"].as_u64(), Some(0));
}

#[test]
fn live_sync_browse_grid_shrinks_when_a_non_selected_image_is_deleted() {
    // Browse `pics` (4 images, selection on index 0 = img-00). Delete img-03 (not selected). The
    // grid count drops to 3 and the selection stays on img-00.
    let (home, images, _empty) = create_browse_home(4);
    let app = TestApp::start_browse_dir(&images, home.path());
    wait_for_browse_listed(&app, 4, Duration::from_secs(8));
    assert_eq!(app.get_state()["browse_grid_selected"].as_u64(), Some(0));
    app.wait_for_watch(&images);

    std::fs::remove_file(images.join("img-03.png")).unwrap();

    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["browse_grid_count"].as_u64() == Some(3)
    });
    assert_eq!(
        state["browse_grid_count"].as_u64(),
        Some(3),
        "a deleted image should vanish from the grid, got {state}"
    );
    // Selection unchanged (img-00 still at index 0).
    assert_eq!(state["browse_grid_selected"].as_u64(), Some(0));
}

#[test]
fn live_sync_browse_grid_keeps_selection_by_path_when_an_earlier_image_is_deleted() {
    // Browse `pics` (4 images), select index 2 (img-02). Delete img-00 (before the selection). The
    // count drops to 3 and the selection tracks img-02 by path — now at index 1.
    let (home, images, _empty) = create_browse_home(4);
    let app = TestApp::start_browse_dir(&images, home.path());
    wait_for_browse_listed(&app, 4, Duration::from_secs(8));

    // Select img-02 (index 2) via the QA hook.
    app.post("/browse/select-grid", "2");
    let state = app.wait_for_state(Duration::from_secs(5), |s| {
        s["browse_grid_selected"].as_u64() == Some(2)
    });
    assert_eq!(state["browse_grid_selected"].as_u64(), Some(2));
    app.wait_for_watch(&images);

    std::fs::remove_file(images.join("img-00.png")).unwrap();

    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["browse_grid_count"].as_u64() == Some(3)
    });
    assert_eq!(state["browse_grid_count"].as_u64(), Some(3));
    // img-02 shifted from index 2 to index 1, but the selection still points at it (by path).
    assert_eq!(
        state["browse_grid_selected"].as_u64(),
        Some(1),
        "the selection should track the same file by path across a delete, got {state}"
    );
}

#[test]
fn live_sync_browse_grid_empties_when_all_images_are_deleted() {
    // Browse a folder of 2 images, delete both → the grid lists zero and the "(No images)" state
    // applies (grid_count 0). The tree stays put.
    let (home, images, _empty) = create_browse_home(2);
    let app = TestApp::start_browse_dir(&images, home.path());
    wait_for_browse_listed(&app, 2, Duration::from_secs(8));
    app.wait_for_watch(&images);

    std::fs::remove_file(images.join("img-00.png")).unwrap();
    std::fs::remove_file(images.join("img-01.png")).unwrap();

    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["browse_grid_count"].as_u64() == Some(0)
    });
    assert_eq!(
        state["browse_grid_count"].as_u64(),
        Some(0),
        "deleting every image should empty the grid, got {state}"
    );
    assert_eq!(state["browse_grid_selected"], serde_json::Value::Null);
    // Still in browse mode (the grid emptying doesn't bounce us to image mode).
    assert_eq!(state["view_mode"].as_str(), Some("browse"));
}
