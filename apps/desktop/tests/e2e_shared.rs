//! The platform-neutral E2E core: every assertion here is about observable state through the QA
//! HTTP server, so it runs anywhere the app runs.
//!
//! Layer 3 of the parity harness (`docs/specs/cross-platform-plan.md`, M0.5). Layer 1 proves a
//! toggle exists on both platforms; this proves it does the same thing. The rules that keep the
//! file neutral, and what still has to be verified off macOS, are in `tests/e2e/mod.rs`.
//!
//! Anything that has to poke a native widget lives in `tests/e2e_macos.rs` instead.

mod e2e;

use std::time::Duration;

use e2e::fixtures::{create_jpeg_with_exif, create_multi_image_dir, create_white_image, write_png};
use e2e::shared::SharedApp;
use image::GenericImageView;

/// The guard that makes the split structural rather than a rule in a doc.
#[test]
fn shared_suite_stays_platform_neutral() {
    e2e::shared::assert_source_is_platform_neutral(include_str!("e2e_shared.rs"));
}

// ── The gate's own tests ─────────────────────────────────────────────────────────────────────
//
// On a Mac every command is `Present`, so the skip and fail branches never run and would ship
// unexercised into the first Windows CI run. A synthetic table exercises all three.

/// A one-entry parity table saying the host does `status` with `Refresh`.
fn parity_table_saying(status: &str, reason: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "entries": [{
            "registry": "Command",
            "name": "Refresh",
            "platforms": [{
                "platform": e2e::shared::HOST_PLATFORM,
                "status": status,
                "reason": reason,
            }],
        }],
    })
}

#[test]
fn a_command_the_host_has_lets_the_test_run() {
    let table = parity_table_saying("done", None);
    assert_eq!(
        e2e::shared::host_coverage(&table, "Refresh"),
        e2e::shared::Coverage::Present
    );
}

#[test]
fn a_command_that_doesnt_apply_here_carries_its_reason_to_the_skip() {
    let table = parity_table_saying("not applicable", Some("No strip to reserve."));
    assert_eq!(
        e2e::shared::host_coverage(&table, "Refresh"),
        e2e::shared::Coverage::NotApplicable("No strip to reserve.".to_string())
    );
}

#[test]
fn a_command_the_host_hasnt_built_is_a_gap_not_a_skip() {
    let table = parity_table_saying("missing", None);
    assert_eq!(
        e2e::shared::host_coverage(&table, "Refresh"),
        e2e::shared::Coverage::Missing
    );
}

#[test]
#[should_panic(expected = "no `CommandKey` called Nonesuch")]
fn a_misspelled_capability_fails_rather_than_waving_the_test_through() {
    let table = parity_table_saying("done", None);
    let _ = e2e::shared::host_coverage(&table, "Nonesuch");
}

#[test]
#[should_panic(expected = "appears in the shared E2E suite")]
fn the_neutrality_guard_rejects_a_platform_branch() {
    // Spelled in two pieces so the needle isn't a substring of this file, which the guard
    // itself scans.
    let offending = format!("#[cfg(target{}os = \"macos\")]", "_");
    e2e::shared::assert_source_is_platform_neutral(&offending);
}

/// Sorted directory indices currently resident in the image cache.
fn cache_indices(state: &serde_json::Value) -> Vec<u64> {
    state["cache_indices"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
        .unwrap_or_default()
}

// ── The state contract ───────────────────────────────────────────────────────────────────────

#[test]
fn app_starts_and_loads_image() {
    let Some(app) = SharedApp::start(&[]) else {
        return;
    };
    let state = app.get_state();
    assert!(state["file"].as_str().unwrap().contains("fixture.png"));
    assert!(state["image_width"].as_u64().unwrap() > 0);
    assert!(state["image_height"].as_u64().unwrap() > 0);
}

#[test]
fn window_geometry_changes_size() {
    let Some(app) = SharedApp::start(&[]) else {
        return;
    };
    let json = serde_json::json!({"width": 400, "height": 300});
    app.post_json("/window-geometry", &json);
    std::thread::sleep(Duration::from_millis(200));
    let state = app.get_state();
    let w = state["window_width"].as_u64().unwrap();
    let h = state["window_height"].as_u64().unwrap();
    assert!(w > 0 && h > 0, "window should have positive dimensions");
}

#[test]
fn refresh_redisplays_image() {
    let Some(app) = SharedApp::start(&["Refresh"]) else {
        return;
    };
    let before = app.get_state();
    app.post("/refresh", "");
    let after = app.get_state();
    assert_eq!(
        before["file"].as_str(),
        after["file"].as_str(),
        "refresh should keep the same file"
    );
}

#[test]
fn navigate_with_single_file() {
    let Some(app) = SharedApp::start(&["NextPreviousImage"]) else {
        return;
    };
    let before = app.get_state();
    // The fixture is the only file in its directory, so navigate should keep it
    app.post("/navigate", "next");
    let after = app.get_state();
    if before["total_files"].as_u64().unwrap() == 1 {
        assert_eq!(before["file"].as_str(), after["file"].as_str());
    }
}

#[test]
fn a_multi_file_launch_opens_the_named_file_at_its_own_index() {
    // `prvw b.png a.png` opens b.png, because that's the one the user named first. The list it
    // navigates is both files in the user's sort order, so b.png sits at slot 2 of 2 — and the
    // index, the title, and the pixels on screen all have to agree about that. Different sizes
    // per file make the pixels observable: `image_width` is whatever actually got displayed.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    let b = dir.path().join("b.png");
    create_white_image(&a, 8, 8);
    create_white_image(&b, 16, 16);

    let Some(app) = SharedApp::start_with_images(&["NextPreviousImage"], &[&b, &a]) else {
        return;
    };

    let state = app.get_state();
    assert!(
        state["file"].as_str().unwrap().contains("b.png"),
        "the named file opens, not whichever sorts first: {state:?}"
    );
    assert_eq!(state["total_files"].as_u64(), Some(2));
    assert_eq!(
        state["index"].as_u64(),
        Some(2),
        "b.png sorts second, so the position reads 2 of 2: {state:?}"
    );
    assert_eq!(
        state["image_width"].as_u64(),
        Some(16),
        "the pixels on screen are b.png's: {state:?}"
    );

    // And the other file is still reachable, as itself.
    app.post("/navigate", "prev");
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(1));
    assert!(
        state["file"].as_str().unwrap().contains("a.png"),
        "previous lands on a.png: {state:?}"
    );
    assert_eq!(
        state["image_width"].as_u64(),
        Some(8),
        "and shows a.png's pixels, not the ones cached for the launch image: {state:?}"
    );
}

// ── Zoom, fit, and the window it fits to ─────────────────────────────────────────────────────

#[test]
fn zoom_in_increases_zoom() {
    let Some(app) = SharedApp::start(&["ZoomIn"]) else {
        return;
    };
    let before = app.get_state()["zoom"].as_f64().unwrap();
    app.post("/zoom-in", "");
    let after = app.get_state()["zoom"].as_f64().unwrap();
    assert!(after > before, "zoom should increase: {before} -> {after}");
}

#[test]
fn zoom_out_decreases_zoom() {
    let Some(app) = SharedApp::start(&["ZoomIn", "ZoomOut"]) else {
        return;
    };
    // First zoom in so we have room to zoom out
    app.post("/zoom-in", "");
    let before = app.get_state()["zoom"].as_f64().unwrap();
    app.post("/zoom-out", "");
    let after = app.get_state()["zoom"].as_f64().unwrap();
    assert!(after < before, "zoom should decrease: {before} -> {after}");
}

#[test]
fn fit_to_window_resets_zoom() {
    let Some(app) = SharedApp::start(&["FitToWindow", "ZoomIn", "AutoFitWindow"]) else {
        return;
    };
    // Disable auto-fit so zoom-in actually changes the zoom level without resizing the window
    app.post("/auto-fit", "off");
    let initial_zoom = app.get_state()["zoom"].as_f64().unwrap();
    app.post("/zoom-in", "");
    app.post("/zoom-in", "");
    let zoomed_in = app.get_state()["zoom"].as_f64().unwrap();
    assert!(
        zoomed_in > initial_zoom,
        "zoom-in should have increased zoom"
    );
    app.post("/zoom", "fit");
    let after_fit = app.get_state()["zoom"].as_f64().unwrap();
    assert!(
        after_fit < zoomed_in,
        "fit should reduce zoom from zoomed-in state: {after_fit} should be < {zoomed_in}"
    );
}

#[test]
fn actual_size_sets_zoom_to_1() {
    let Some(app) = SharedApp::start(&["ActualSize"]) else {
        return;
    };
    app.post("/zoom", "actual");
    let zoom = app.get_state()["zoom"].as_f64().unwrap();
    assert!(
        (zoom - 1.0).abs() < 0.01,
        "actual size should be zoom=1.0, got {zoom}"
    );
}

#[test]
fn auto_fit_toggle() {
    let Some(app) = SharedApp::start(&["AutoFitWindow"]) else {
        return;
    };
    let before = app.get_state()["auto_fit_window"].as_bool().unwrap();
    let new_value = !before;
    app.post("/auto-fit", if new_value { "on" } else { "off" });
    let after = app.get_state()["auto_fit_window"].as_bool().unwrap();
    assert_eq!(after, new_value);
}

#[test]
fn scroll_to_zoom_toggle() {
    let Some(app) = SharedApp::start(&["ScrollToZoom"]) else {
        return;
    };
    let before = app.get_state()["scroll_to_zoom"].as_bool().unwrap();
    let new_value = !before;
    app.post("/scroll-to-zoom", if new_value { "on" } else { "off" });
    let after = app.get_state()["scroll_to_zoom"].as_bool().unwrap();
    assert_eq!(after, new_value);
}

#[test]
fn enabling_auto_fit_refits_zoom_to_resized_window() {
    // With auto-fit off, the window can be a very different size than the image's auto-fit
    // target. Enabling auto-fit resizes the window AND must re-fit zoom against the NEW
    // window size, not the stale (pre-resize) one. Pre-fix, zoom was fit against the stale
    // (larger) window, so after the window shrank the image stayed zoomed in and overflowed.
    let Some(app) = SharedApp::start(&["AutoFitWindow", "ZoomIn"]) else {
        return;
    };
    app.post("/auto-fit", "off");
    // Force a window much larger than the 1024x1024 fixture's natural fit, then zoom in so
    // we're clearly above fit. (Auto-fit off means neither step resizes the window.)
    app.post_json(
        "/window-geometry",
        &serde_json::json!({"width": 1400, "height": 1400}),
    );
    std::thread::sleep(Duration::from_millis(200));
    app.post("/zoom-in", "");
    app.post("/zoom-in", "");

    app.post("/auto-fit", "on");
    std::thread::sleep(Duration::from_millis(200));
    let s = app.get_state();
    let win_w = s["window_width"].as_f64().unwrap();
    let win_h = s["window_height"].as_f64().unwrap();
    let rw = s["image_render_width"].as_f64().unwrap();
    let rh = s["image_render_height"].as_f64().unwrap();
    // Auto-fit promises the image fits within the window. A small slack absorbs rounding.
    assert!(
        rw <= win_w + 2.0 && rh <= win_h + 2.0,
        "image should fit the window after enabling auto-fit, not overflow it: \
         render {rw}x{rh}, window {win_w}x{win_h}"
    );
}

#[test]
fn fullscreen_respects_enlarge_setting_even_with_auto_fit_on() {
    // Auto-fit can't resize the window in fullscreen (the window is the whole screen), so it's
    // inert there and the fit/enlarge rules govern instead. A small image with "Enlarge small
    // images" OFF must stay at actual size in fullscreen, NOT be blown up, and toggling
    // enlarge while in fullscreen must take effect immediately.
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("small.png");
    create_white_image(&img_path, 64, 64);
    let Some(app) = SharedApp::start_with_image(
        &["Fullscreen", "AutoFitWindow", "EnlargeSmallImages"],
        &img_path,
    ) else {
        return;
    };

    app.post("/auto-fit", "on");
    app.post("/fullscreen", "on");
    let s = app.wait_for_state(Duration::from_secs(5), |s| {
        s["fullscreen"].as_bool() == Some(true)
    });
    assert_eq!(
        s["fullscreen"].as_bool(),
        Some(true),
        "fullscreen should engage in the test harness"
    );

    // Enlarge OFF in fullscreen: the small image must stay at actual size (~100%), not be
    // force-fit by auto-fit. (Pre-fix, auto-fit overrode this and kept it enlarged.)
    app.post("/enlarge-small", "off");
    let s = app.wait_for_state(Duration::from_secs(8), |s| {
        (s["zoom"].as_f64().unwrap_or(0.0) - 1.0).abs() < 0.05
    });
    let zoom_no_enlarge = s["zoom"].as_f64().unwrap();
    assert!(
        (zoom_no_enlarge - 1.0).abs() < 0.05,
        "small image must stay at 100% in fullscreen when enlarge is off, got {zoom_no_enlarge}"
    );

    // Toggling enlarge ON while in fullscreen must take effect and scale the image up.
    app.post("/enlarge-small", "on");
    let s = app.wait_for_state(Duration::from_secs(8), |s| {
        s["zoom"].as_f64().unwrap_or(0.0) > 1.5
    });
    let zoom_enlarged = s["zoom"].as_f64().unwrap();
    assert!(
        zoom_enlarged > 1.5,
        "enlarge on in fullscreen should scale the small image up, got {zoom_enlarged}"
    );
}

// ── Title bar ────────────────────────────────────────────────────────────────────────────────
//
// The `TitleBar` capability is what makes these shared rather than macOS-only. Prvw draws behind
// a transparent title bar on macOS and reserves a strip for it; Windows and Linux decorations sit
// outside the surface, so the registry calls the whole action `NotApplicable` there and these
// skip with that reason. If a platform ever grows the same idea, they start running on it with no
// edit here.

#[test]
fn title_bar_toggle() {
    let Some(app) = SharedApp::start(&["TitleBar"]) else {
        return;
    };
    let before = app.get_state()["title_bar"].as_bool().unwrap();
    let new_value = !before;
    app.post("/title-bar", if new_value { "on" } else { "off" });
    let after = app.get_state()["title_bar"].as_bool().unwrap();
    assert_eq!(after, new_value);
}

/// Both title-bar geometry tests below open this instead of the default fixture.
///
/// Auto-fit caps the window at 90% of the screen, and once that cap binds the window is already
/// as tall as it may get, so toggling the title bar keeps the height and resizes the image
/// instead. That's the auto-fit contract working, but it makes the strip invisible in window
/// geometry. The default 1024x1024 fixture hits the cap on a screen shorter than about 1,173
/// logical pixels, which is every GitHub macOS runner (1024x768) and plenty of real Macs, so
/// these tests need an image no screen can cap. 320 leaves room above the 200px minimum window
/// dimension and needs only a 392-pixel-tall screen to stay uncapped.
const TITLE_BAR_FIXTURE_DIM: u32 = 320;

/// With auto-fit ON, toggling the title bar changes window height by the title bar height.
#[test]
fn title_bar_toggle_resizes_window() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("uncappable.png");
    create_white_image(&img_path, TITLE_BAR_FIXTURE_DIM, TITLE_BAR_FIXTURE_DIM);

    let Some(app) = SharedApp::start_with_image(&["TitleBar", "AutoFitWindow"], &img_path) else {
        return;
    };
    // Title bar is ON by default, auto-fit is ON by default
    assert!(app.get_state()["title_bar"].as_bool().unwrap());
    assert!(app.get_state()["auto_fit_window"].as_bool().unwrap());

    let height_on = app.get_state()["window_height"].as_u64().unwrap();
    // The strip the app reserves, straight from `/state` rather than a constant this file would
    // have to keep in step with `main.rs`.
    let strip = app.get_state()["content_offset_y"].as_f64().unwrap();
    assert!(strip > 0.0, "a reserved strip is what this test measures");

    // Auto-fit sizes the window to the image plus the strip. Asserting the absolute height (not
    // only the delta) is what makes a screen-capped window fail loudly here rather than silently
    // turn the delta below into a comparison of two capped numbers.
    assert_eq!(
        height_on as f64,
        TITLE_BAR_FIXTURE_DIM as f64 + strip,
        "auto-fit should size the window to the image plus the strip, uncapped by the screen"
    );

    // Toggle title bar OFF
    app.post("/title-bar", "off");
    std::thread::sleep(Duration::from_millis(200));

    let height_off = app.get_state()["window_height"].as_u64().unwrap();

    assert_eq!(
        height_on as f64 - height_off as f64,
        strip,
        "Window should shrink by {strip}px when title bar is toggled OFF: {height_on} -> {height_off}"
    );
}

/// Zoom should stay the same when toggling the title bar (image stays same size).
#[test]
fn title_bar_toggle_preserves_zoom() {
    // Same reason as `title_bar_toggle_resizes_window`: on a capped window the image is what
    // resizes, so zoom would legitimately change and this test would measure the cap instead.
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("uncappable.png");
    create_white_image(&img_path, TITLE_BAR_FIXTURE_DIM, TITLE_BAR_FIXTURE_DIM);

    let Some(app) = SharedApp::start_with_image(&["TitleBar", "AutoFitWindow"], &img_path) else {
        return;
    };
    assert!(app.get_state()["title_bar"].as_bool().unwrap());

    let zoom_on = app.get_state()["zoom"].as_f64().unwrap();

    app.post("/title-bar", "off");
    std::thread::sleep(Duration::from_millis(200));

    let zoom_off = app.get_state()["zoom"].as_f64().unwrap();

    assert!(
        (zoom_on - zoom_off).abs() < 0.01,
        "Zoom should not change when toggling title bar: {zoom_on} -> {zoom_off}"
    );
}

/// Title bar ON: the screenshot renders the image, and the transform stays sane.
///
/// The screenshot uses the same transform as the window but renders without the viewport, so at
/// fit zoom the image fills the whole surface either way. That means pixels can't tell title-bar
/// ON from OFF; what they can tell is that the image is still being rendered, which is the
/// regression this catches.
#[test]
fn title_bar_on_screenshot_has_reserved_area() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("white.png");
    create_white_image(&img_path, 800, 800);

    let Some(app) = SharedApp::start_with_image(&["TitleBar"], &img_path) else {
        return;
    };
    // Title bar is ON by default
    assert!(app.get_state()["title_bar"].as_bool().unwrap());

    let screenshot = app.get_screenshot();
    let (sw, sh) = (screenshot.width(), screenshot.height());

    let center_pixel = screenshot.get_pixel(sw / 2, sh / 2);
    assert!(
        center_pixel[0] > 200 && center_pixel[1] > 200 && center_pixel[2] > 200,
        "Center pixel should be white (image content), got {center_pixel:?}"
    );

    let total_white: u64 = (0..sh)
        .map(|y| {
            let p = screenshot.get_pixel(sw / 2, y);
            if p[0] > 200 { 1u64 } else { 0 }
        })
        .sum();
    assert!(
        total_white > (sh as u64) / 2,
        "Most of the screenshot should be white image, got {total_white}/{sh} white rows"
    );
}

/// Title bar OFF: screenshot should show image content at y=0 (no reserved area).
#[test]
fn title_bar_off_screenshot_no_reserved_area() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("white.png");
    create_white_image(&img_path, 800, 800);

    let Some(app) = SharedApp::start_with_image(&["TitleBar"], &img_path) else {
        return;
    };
    // Toggle title bar OFF
    app.post("/title-bar", "off");
    std::thread::sleep(Duration::from_millis(200));

    let screenshot = app.get_screenshot();
    let sw = screenshot.width();

    // With title bar OFF, the image should fill the entire window. The very first row
    // of the screenshot should be white (image content, not a black reserved area).
    let top_pixel = screenshot.get_pixel(sw / 2, 1);
    assert!(
        top_pixel[0] > 200 && top_pixel[1] > 200 && top_pixel[2] > 200,
        "With title bar OFF, pixel at y=1 should be white (image), got {top_pixel:?}"
    );
}

// ── Overlays: histogram and EXIF ─────────────────────────────────────────────────────────────

/// Toggling the histogram via the H key flips `histogram_visible` in shared state.
#[test]
fn histogram_h_toggles_visibility() {
    let Some(app) = SharedApp::start(&["Histogram"]) else {
        return;
    };
    assert_eq!(
        app.get_state()["histogram_visible"].as_bool(),
        Some(false),
        "histogram is off by default"
    );

    app.post("/key", "h");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        app.get_state()["histogram_visible"].as_bool(),
        Some(true),
        "H key turns histogram on"
    );

    app.post("/key", "h");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        app.get_state()["histogram_visible"].as_bool(),
        Some(false),
        "H key turns histogram off again"
    );
}

/// With the histogram visible, moving the cursor into the plot rect updates
/// `histogram_hover_bin`. The exact bin depends on layout, so we just assert
/// it is `Some(_)` when inside and `None` outside.
#[test]
fn histogram_hover_bin_updates() {
    let Some(app) = SharedApp::start(&["Histogram"]) else {
        return;
    };
    let _ = app.mcp_call("histogram", serde_json::json!({}));
    // No `sleep` here: hover uses the deterministic `plot_rect_for` helper, so it works without
    // a prior render. The MCP call already syncs back through `update_shared_state`, so the
    // state read below sees `histogram_visible == true` immediately.
    assert_eq!(app.get_state()["histogram_visible"].as_bool(), Some(true));

    // The histogram panel sits in the top-right. The plot rect lives at approximately
    // (window_width - 256 - 7 + 10, content_offset_y + 7 + 22) and is 236 wide × 70 tall. Pick a
    // point ~halfway across. `content_offset_y` comes from `/state` because how much the app
    // reserves at the top is exactly what differs between platforms.
    let state = app.get_state();
    let window_width = state["window_width"].as_u64().unwrap() as f64;
    let content_offset_y = state["content_offset_y"].as_f64().unwrap();
    let plot_x = window_width - 256.0 - 7.0 + 10.0;
    let plot_y = content_offset_y + 7.0 + 22.0;
    // Mid-plot.
    let cursor_x = plot_x + 118.0;
    let cursor_y = plot_y + 30.0;

    let _ = app.mcp_call(
        "set_cursor_position",
        serde_json::json!({ "x": cursor_x, "y": cursor_y }),
    );
    let state = app.wait_for_state(Duration::from_secs(2), |s| {
        s["histogram_hover_bin"].is_u64()
    });
    assert!(
        state["histogram_hover_bin"].is_u64(),
        "cursor inside the plot rect should produce a hover bin, got state: {state}"
    );

    // Move the cursor far away — hover bin should clear.
    let _ = app.mcp_call(
        "set_cursor_position",
        serde_json::json!({ "x": 5.0, "y": 5.0 }),
    );
    let state = app.wait_for_state(Duration::from_secs(2), |s| {
        s["histogram_hover_bin"].is_null()
    });
    assert!(
        state["histogram_hover_bin"].is_null(),
        "cursor outside the plot rect should clear hover bin, got state: {state}"
    );
}

#[test]
fn exif_e_toggles_with_exif_jpeg() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = create_jpeg_with_exif(dir.path());

    let Some(app) = SharedApp::start_with_image(&["ExifInfo"], &img_path) else {
        return;
    };

    let s0 = app.get_state();
    assert_eq!(s0["exif_visible"].as_bool(), Some(false));
    assert_eq!(
        s0["exif_present"].as_bool(),
        Some(true),
        "JPEG with EXIF should report exif_present=true"
    );

    app.post("/key", "e");
    std::thread::sleep(Duration::from_millis(150));
    let s1 = app.get_state();
    assert_eq!(s1["exif_visible"].as_bool(), Some(true));
    assert_eq!(s1["exif_present"].as_bool(), Some(true));

    app.post("/key", "e");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(app.get_state()["exif_visible"].as_bool(), Some(false));
}

#[test]
fn exif_e_on_png_marks_not_present_but_no_panic() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("no-exif.png");
    create_white_image(&img_path, 64, 64);

    let Some(app) = SharedApp::start_with_image(&["ExifInfo"], &img_path) else {
        return;
    };

    app.post("/key", "e");
    std::thread::sleep(Duration::from_millis(150));
    let state = app.get_state();
    assert_eq!(state["exif_visible"].as_bool(), Some(true));
    assert_eq!(
        state["exif_present"].as_bool(),
        Some(false),
        "PNG has no EXIF, exif_present should be false"
    );

    // Confirm the app is still alive and rendering by hitting another endpoint.
    let screenshot = app.get_screenshot();
    assert!(screenshot.width() > 0 && screenshot.height() > 0);
}

#[test]
fn exif_visibility_persists_across_settings_reload() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = create_jpeg_with_exif(dir.path());

    let Some(app) = SharedApp::start_with_image(&["ExifInfo"], &img_path) else {
        return;
    };
    app.post("/key", "e");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(app.get_state()["exif_visible"].as_bool(), Some(true));

    // Read settings.json from the per-test data dir to confirm the flag landed.
    let settings_path = app.settings_path();
    let json = std::fs::read_to_string(&settings_path).expect("settings file should exist");
    assert!(
        json.contains("\"exif_visible\": true"),
        "exif_visible should persist to settings.json, got: {json}"
    );
}

#[test]
fn histogram_and_exif_both_visible_independent_flags() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = create_jpeg_with_exif(dir.path());

    let Some(app) = SharedApp::start_with_image(&["Histogram", "ExifInfo"], &img_path) else {
        return;
    };
    app.post("/key", "h");
    std::thread::sleep(Duration::from_millis(120));
    app.post("/key", "e");
    std::thread::sleep(Duration::from_millis(120));

    let state = app.get_state();
    assert_eq!(state["histogram_visible"].as_bool(), Some(true));
    assert_eq!(state["exif_visible"].as_bool(), Some(true));
    assert_eq!(state["exif_present"].as_bool(), Some(true));
}

/// Toggling the histogram and EXIF overlays at a very narrow window width must not
/// crash the renderer. The overlays don't currently clamp their geometry against
/// `window_width`; this test is the safety net that proves the trivial
/// render-to-negative-x path is harmless.
#[test]
fn narrow_window_overlays_dont_crash() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = create_jpeg_with_exif(dir.path());

    let Some(app) = SharedApp::start_with_image(
        &[
            "Histogram",
            "ExifInfo",
            "AutoFitWindow",
            "NextPreviousImage",
        ],
        &img_path,
    ) else {
        return;
    };

    // Disable auto-fit so the next window-geometry change actually sticks.
    app.post("/auto-fit", "off");
    std::thread::sleep(Duration::from_millis(120));

    // Squeeze the window down to ~150 logical pixels. The histogram panel itself is
    // 256 pixels wide, so its layout math will produce a negative x — exactly the
    // path we want to exercise.
    let geometry = serde_json::json!({"width": 150, "height": 400});
    app.post_json("/window-geometry", &geometry);
    std::thread::sleep(Duration::from_millis(200));

    // Histogram on, EXIF on, navigate, navigate back, then both off. Each step
    // round-trips through the QA server, so a panic on the main thread would
    // surface as a connection error here.
    app.post("/key", "h");
    let s = app.wait_for_state(Duration::from_secs(2), |s| {
        s["histogram_visible"].as_bool() == Some(true)
    });
    assert_eq!(s["histogram_visible"].as_bool(), Some(true));

    app.post("/key", "e");
    let s = app.wait_for_state(Duration::from_secs(2), |s| {
        s["exif_visible"].as_bool() == Some(true)
    });
    assert_eq!(s["exif_visible"].as_bool(), Some(true));

    // Navigation is fire-and-forget here — we just want to exercise the code
    // paths without crashing. The single-image temp dir means /navigate is a
    // no-op anyway, so there's nothing observable to wait for.
    app.post("/navigate", "next");
    app.post("/navigate", "prev");

    app.post("/key", "h");
    let s = app.wait_for_state(Duration::from_secs(2), |s| {
        s["histogram_visible"].as_bool() == Some(false)
    });
    assert_eq!(s["histogram_visible"].as_bool(), Some(false));

    app.post("/key", "e");
    let final_state = app.wait_for_state(Duration::from_secs(2), |s| {
        s["exif_visible"].as_bool() == Some(false)
    });
    assert_eq!(final_state["histogram_visible"].as_bool(), Some(false));
    assert_eq!(final_state["exif_visible"].as_bool(), Some(false));
}

// ── Navigation: loop, Home, End ──────────────────────────────────────────────────────────────

#[test]
fn loop_l_toggles_visibility_in_state() {
    let (_dir, first) = create_multi_image_dir(5);
    let Some(app) = SharedApp::start_with_image(&["LoopNavigation"], &first) else {
        return;
    };

    assert_eq!(
        app.get_state()["loop_navigation"].as_bool(),
        Some(false),
        "loop navigation defaults to off"
    );

    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        app.get_state()["loop_navigation"].as_bool(),
        Some(true),
        "L key turns loop navigation on"
    );

    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        app.get_state()["loop_navigation"].as_bool(),
        Some(false),
        "L key turns loop navigation off again"
    );
}

#[test]
fn loop_navigation_wraps_next_at_last() {
    let (_dir, first) = create_multi_image_dir(5);
    let Some(app) = SharedApp::start_with_image(&["LoopNavigation", "NextPreviousImage"], &first)
    else {
        return;
    };

    // Turn loop on first.
    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));

    // Navigate forward four times to reach the last image (index 4 of 5).
    for _ in 0..4 {
        app.post("/navigate", "next");
    }
    let state = app.wait_for_state(Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(5) // 1-based index = 5 -> 0-based 4
    });
    assert_eq!(state["index"].as_u64(), Some(5));
    assert_eq!(state["total_files"].as_u64(), Some(5));

    // Next from last wraps to first.
    app.post("/navigate", "next");
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(1));
    assert_eq!(
        state["index"].as_u64(),
        Some(1),
        "next at last wraps to first"
    );

    // Previous from first wraps to last.
    app.post("/navigate", "prev");
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(5));
    assert_eq!(
        state["index"].as_u64(),
        Some(5),
        "previous at first wraps to last"
    );
}

#[test]
fn loop_off_halts_at_edge() {
    let (_dir, first) = create_multi_image_dir(5);
    let Some(app) = SharedApp::start_with_image(&["LoopNavigation", "NextPreviousImage"], &first)
    else {
        return;
    };
    // Loop is off by default. Confirm.
    assert_eq!(app.get_state()["loop_navigation"].as_bool(), Some(false));

    // Walk to the last image.
    for _ in 0..4 {
        app.post("/navigate", "next");
    }
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(5));
    assert_eq!(state["index"].as_u64(), Some(5));

    // Next at the last image with loop off should leave the index unchanged.
    app.post("/navigate", "next");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        app.get_state()["index"].as_u64(),
        Some(5),
        "next at last with loop off halts at last"
    );

    // Walk back to the first image.
    for _ in 0..4 {
        app.post("/navigate", "prev");
    }
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(1));
    assert_eq!(state["index"].as_u64(), Some(1));

    // Previous at the first image with loop off halts.
    app.post("/navigate", "prev");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        app.get_state()["index"].as_u64(),
        Some(1),
        "previous at first with loop off halts at first"
    );
}

#[test]
fn loop_toggle_on_triggers_preload_of_wrap_indices() {
    let (_dir, first) = create_multi_image_dir(6);
    let Some(app) = SharedApp::start_with_image(
        &["LoopNavigation", "NextPreviousImage", "PreloadNeighbors"],
        &first,
    ) else {
        return;
    };

    // Walk to the last image (index 5 of 6).
    for _ in 0..5 {
        app.post("/navigate", "next");
    }
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(6));
    assert_eq!(state["index"].as_u64(), Some(6));

    // With loop OFF, the cache must not contain wrap-side indices 0 or 1.
    let state = app.wait_for_state(Duration::from_secs(3), |s| {
        let idx = cache_indices(s);
        // Wait for the active window (3, 4, 5) to be in the cache.
        idx.contains(&5) && !idx.contains(&0) && !idx.contains(&1)
    });
    let before = cache_indices(&state);
    assert!(
        !before.contains(&0) && !before.contains(&1),
        "wrap-side indices should not be cached before loop on, got {before:?}"
    );

    // Toggle loop ON. Wrap-side indices 0 and 1 should now warm.
    app.post("/key", "l");
    let state = app.wait_for_state(Duration::from_secs(5), |s| {
        let idx = cache_indices(s);
        idx.contains(&0) && idx.contains(&1)
    });
    let after = cache_indices(&state);
    assert!(
        after.contains(&0) && after.contains(&1),
        "wrap-side indices should be cached after loop on, got {after:?}"
    );
}

#[test]
fn loop_toggle_off_evicts_wrap_indices() {
    let (_dir, first) = create_multi_image_dir(6);
    let Some(app) = SharedApp::start_with_image(
        &["LoopNavigation", "NextPreviousImage", "PreloadNeighbors"],
        &first,
    ) else {
        return;
    };

    // Loop on first so the wrap-side preloads run.
    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));

    // Walk to the last image.
    for _ in 0..5 {
        app.post("/navigate", "next");
    }
    let state = app.wait_for_state(Duration::from_secs(5), |s| {
        let idx = cache_indices(s);
        s["index"].as_u64() == Some(6) && idx.contains(&0) && idx.contains(&1)
    });
    let before = cache_indices(&state);
    assert!(
        before.contains(&0) && before.contains(&1),
        "wrap-side indices should be cached with loop on at last, got {before:?}"
    );

    // Toggle loop OFF. Wrap-side indices should be dropped from the cache.
    app.post("/key", "l");
    let state = app.wait_for_state(Duration::from_secs(5), |s| {
        let idx = cache_indices(s);
        !idx.contains(&0) && !idx.contains(&1)
    });
    let after = cache_indices(&state);
    assert!(
        !after.contains(&0) && !after.contains(&1),
        "wrap-side indices should be evicted after loop off, got {after:?}"
    );
}

#[test]
fn home_key_jumps_to_first() {
    let (_dir, first) = create_multi_image_dir(5);
    let Some(app) = SharedApp::start_with_image(&["GoToFirst", "NextPreviousImage"], &first) else {
        return;
    };

    // Walk to a middle image (index 2 of 5).
    for _ in 0..2 {
        app.post("/navigate", "next");
    }
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(3));
    assert_eq!(state["index"].as_u64(), Some(3));

    // Press Home — jumps to first.
    app.post("/key", "Home");
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(1));
    assert_eq!(
        state["index"].as_u64(),
        Some(1),
        "Home jumps to first image"
    );
}

#[test]
fn end_key_jumps_to_last() {
    let (_dir, first) = create_multi_image_dir(5);
    let Some(app) = SharedApp::start_with_image(&["GoToLast"], &first) else {
        return;
    };
    assert_eq!(app.get_state()["index"].as_u64(), Some(1));

    app.post("/key", "End");
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(5));
    assert_eq!(state["index"].as_u64(), Some(5), "End jumps to last image");
    assert_eq!(state["total_files"].as_u64(), Some(5));
}

#[test]
fn home_at_first_is_noop() {
    let (_dir, first) = create_multi_image_dir(5);
    let Some(app) = SharedApp::start_with_image(&["GoToFirst"], &first) else {
        return;
    };
    assert_eq!(app.get_state()["index"].as_u64(), Some(1));

    app.post("/key", "Home");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        app.get_state()["index"].as_u64(),
        Some(1),
        "Home at first stays at first"
    );
}

#[test]
fn end_at_last_is_noop() {
    let (_dir, first) = create_multi_image_dir(5);
    let Some(app) = SharedApp::start_with_image(&["GoToLast"], &first) else {
        return;
    };

    // Walk to the last image first.
    app.post("/key", "End");
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(5));
    assert_eq!(state["index"].as_u64(), Some(5));

    // End again — should stay.
    app.post("/key", "End");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        app.get_state()["index"].as_u64(),
        Some(5),
        "End at last stays at last"
    );
}

#[test]
fn home_with_loop_on_still_jumps_to_first() {
    let (_dir, first) = create_multi_image_dir(5);
    let Some(app) = SharedApp::start_with_image(
        &["GoToFirst", "LoopNavigation", "NextPreviousImage"],
        &first,
    ) else {
        return;
    };

    // Loop on.
    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(app.get_state()["loop_navigation"].as_bool(), Some(true));

    // Walk to middle.
    for _ in 0..2 {
        app.post("/navigate", "next");
    }
    let _ = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(3));

    // Home jumps to absolute first regardless of loop.
    app.post("/key", "Home");
    let state = app.wait_for_state(Duration::from_secs(3), |s| s["index"].as_u64() == Some(1));
    assert_eq!(
        state["index"].as_u64(),
        Some(1),
        "Home with loop on still jumps to first"
    );
}

#[test]
fn loop_persists_across_settings_reload() {
    let (_dir, first) = create_multi_image_dir(3);
    let Some(app) = SharedApp::start_with_image(&["LoopNavigation"], &first) else {
        return;
    };
    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(app.get_state()["loop_navigation"].as_bool(), Some(true));

    let settings_path = app.settings_path();
    let json = std::fs::read_to_string(&settings_path).expect("settings file should exist");
    assert!(
        json.contains("\"loop_navigation\": true"),
        "loop_navigation should persist to settings.json, got: {json}"
    );
}

// ── Slideshow ────────────────────────────────────────────────────────────────────────────────

#[test]
fn slideshow_s_starts_and_stops_it() {
    let (_dir, first) = create_multi_image_dir(3);
    let Some(app) = SharedApp::start_with_image(&["Slideshow"], &first) else {
        return;
    };

    assert_eq!(
        app.get_state()["slideshow_running"].as_bool(),
        Some(false),
        "the slideshow starts stopped"
    );

    app.post("/key", "s");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        app.get_state()["slideshow_running"].as_bool(),
        Some(true),
        "bare S starts the slideshow"
    );

    app.post("/key", "s");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        app.get_state()["slideshow_running"].as_bool(),
        Some(false),
        "bare S stops it again"
    );
}

// ── The settings window ──────────────────────────────────────────────────────────────────────
//
// Two platforms build one, out of two different toolkits: an AppKit window with a sidebar on
// macOS (`settings::window`) and a Win32 tabbed dialog on Windows (`settings::windows`). None of
// what's asserted here is about either, which is the point: opening it, switching to a section,
// and closing it are the same three things everywhere.
//
// Opening it is also where each platform's own parity audit runs, comparing what the window
// built against what `parity::setting_keys` declares. A `Present` nobody built fails the debug
// assertion inside `check_parity`, and the app dies with it, so these tests are what surface a
// settings surface that doesn't match its declaration.

#[test]
fn settings_opens_and_closes() {
    let Some(app) = SharedApp::start(&["Settings"]) else {
        return;
    };
    app.post("/show-settings", "");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        app.get_state()["file"].as_str().is_some(),
        "the app still answers with the settings window open"
    );
    app.post("/close-settings", "");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        app.get_state()["file"].as_str().is_some(),
        "and it still answers after closing it"
    );
}

#[test]
fn settings_switches_between_sections() {
    let Some(app) = SharedApp::start(&["Settings"]) else {
        return;
    };
    // Both ends of the list, so a platform that only ever shows its first panel is caught.
    for section in ["file_associations", "raw", "general"] {
        app.post("/show-settings", section);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            app.get_state()["file"].as_str().is_some(),
            "the app survived switching to {section}"
        );
    }
    app.post("/close-settings", "");
}

/// The settings window is modeless on both platforms, and this is what that has to mean.
///
/// It's the assertion the whole Windows settings design turns on. A Win32 modal dialog doesn't
/// crash the way an AppKit modal does; it starves winit's message pump, so `about_to_wait` stops
/// running and every `ControlFlow::WaitUntil` timer stops with it. The slideshow's timer is the
/// visible one, so a slideshow that keeps advancing with the window open is the proof that no
/// nested message loop was opened. macOS has the same rule for a different reason
/// (`AGENTS.md`), so the test belongs to both.
#[test]
fn the_settings_window_doesnt_stop_the_slideshow() {
    // Long enough for the default four-second interval to fire at least twice.
    const WATCH: Duration = Duration::from_secs(10);

    let (_dir, first) = create_multi_image_dir(6);
    let Some(app) = SharedApp::start_with_image(&["Settings", "Slideshow"], &first) else {
        return;
    };

    app.post("/key", "s");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        app.get_state()["slideshow_running"].as_bool(),
        Some(true),
        "the slideshow is running before the window opens"
    );
    let started_at = app.get_state()["index"].as_u64().unwrap();

    app.post("/show-settings", "");

    let deadline = std::time::Instant::now() + WATCH;
    let mut advanced = false;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        if app.get_state()["index"].as_u64().unwrap() != started_at {
            advanced = true;
            break;
        }
    }
    app.post("/close-settings", "");
    assert!(
        advanced,
        "the slideshow stopped advancing while the settings window was open, which is what a \
         nested message loop looks like"
    );
}

// ── Live folder sync (image mode) ────────────────────────────────────────────────────────────
//
// These drive the real filesystem watcher: open an image, then mutate its folder from the
// shell-side (the test process) and poll `/state` for the sequence to update. `folder_watch`
// rides the `notify` crate, whose backend is FSEvents, `ReadDirectoryChangesW`, or inotify
// depending on the host, so this is one behaviour with three implementations underneath and
// exactly the sort of thing layer 3 is for. The backends have real latency, so the waits are
// generous, and none of them reports what happened before the stream started, so each test calls
// `wait_for_watch` before touching its folder.
//
// No capability is declared: `folder_watch` carries no platform fork of its own, and the
// navigation these observe is `NextPreviousImage`, which every platform has.

#[test]
fn live_sync_added_image_grows_the_sequence() {
    let (dir, first) = create_multi_image_dir(3); // img-00..img-02
    let Some(app) = SharedApp::start_with_image(&[], &first) else {
        return;
    };
    assert_eq!(app.get_state()["total_files"].as_u64(), Some(3));
    app.wait_for_watch(dir.path());

    // Add a 4th image to the watched folder.
    write_png(&dir.path().join("img-03.png"), 9);

    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["total_files"].as_u64() == Some(4)
    });
    assert_eq!(
        state["total_files"].as_u64(),
        Some(4),
        "added image should appear in the sequence, got {state}"
    );
    // Current image is unchanged (still img-00, 1-based index 1).
    assert!(state["file"].as_str().unwrap().contains("img-00"));
    assert_eq!(state["index"].as_u64(), Some(1));
}

#[test]
fn live_sync_delete_non_current_drops_it_and_keeps_current() {
    let (dir, first) = create_multi_image_dir(3); // img-00 (current) .. img-02
    let Some(app) = SharedApp::start_with_image(&[], &first) else {
        return;
    };
    assert_eq!(app.get_state()["total_files"].as_u64(), Some(3));
    app.wait_for_watch(dir.path());

    // Delete img-02 (not the current image).
    std::fs::remove_file(dir.path().join("img-02.png")).unwrap();

    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["total_files"].as_u64() == Some(2)
    });
    assert_eq!(state["total_files"].as_u64(), Some(2));
    // Current image still img-00.
    assert!(state["file"].as_str().unwrap().contains("img-00"));
    assert_eq!(state["index"].as_u64(), Some(1));
}

#[test]
fn live_sync_delete_current_navigates_to_next() {
    let (dir, first) = create_multi_image_dir(3); // img-00 (current) .. img-02
    let Some(app) = SharedApp::start_with_image(&[], &first) else {
        return;
    };
    app.wait_for_watch(dir.path());

    // Delete the current image (img-00). Should navigate to the next (img-01).
    std::fs::remove_file(dir.path().join("img-00.png")).unwrap();

    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["file"]
            .as_str()
            .map(|f| f.contains("img-01"))
            .unwrap_or(false)
    });
    assert!(
        state["file"].as_str().unwrap().contains("img-01"),
        "deleting the current image should navigate to the next, got {state}"
    );
    assert_eq!(state["total_files"].as_u64(), Some(2));
}

#[test]
fn live_sync_delete_last_image_shows_empty_state() {
    let dir = tempfile::tempdir().unwrap();
    let only = dir.path().join("only.png");
    write_png(&only, 5);
    let Some(app) = SharedApp::start_with_image(&[], &only) else {
        return;
    };
    assert_eq!(app.get_state()["total_files"].as_u64(), Some(1));
    app.wait_for_watch(dir.path());

    // Delete the only image → image-mode "(No images)" empty state.
    std::fs::remove_file(&only).unwrap();

    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["no_images"].as_bool() == Some(true)
    });
    assert_eq!(
        state["no_images"].as_bool(),
        Some(true),
        "deleting the last image should enter the (No images) empty state, got {state}"
    );
    assert!(
        state["file"].is_null(),
        "empty state clears the current file"
    );
    assert_eq!(state["total_files"].as_u64(), Some(0));
    // `empty_state` names which empty state it is, so the launch one ("nothing_open", the
    // window a no-argument launch puts up) can't be mistaken for this one.
    assert_eq!(state["empty_state"].as_str(), Some("no_images"));
}

#[test]
fn live_sync_empty_state_recovers_when_an_image_appears() {
    let dir = tempfile::tempdir().unwrap();
    let only = dir.path().join("only.png");
    write_png(&only, 5);
    let Some(app) = SharedApp::start_with_image(&[], &only) else {
        return;
    };
    app.wait_for_watch(dir.path());

    std::fs::remove_file(&only).unwrap();
    app.wait_for_state(Duration::from_secs(8), |s| {
        s["no_images"].as_bool() == Some(true)
    });

    // A new image appears in the (still-watched) folder → empty state clears, image opens.
    write_png(&dir.path().join("fresh.png"), 7);
    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["no_images"].as_bool() == Some(false) && s["total_files"].as_u64() == Some(1)
    });
    assert_eq!(state["no_images"].as_bool(), Some(false));
    assert!(
        state["empty_state"].is_null(),
        "an image on screen is no empty state at all, got {state}"
    );
    assert_eq!(state["total_files"].as_u64(), Some(1));
    assert!(state["file"].as_str().unwrap().contains("fresh"));
}

#[test]
fn live_sync_modify_current_re_decodes_in_place() {
    let (dir, first) = create_multi_image_dir(3); // img-00 (current) .. img-02
    let Some(app) = SharedApp::start_with_image(&[], &first) else {
        return;
    };
    // img-00 is 8x8 (create_multi_image_dir). Confirm.
    assert_eq!(app.get_state()["image_width"].as_u64(), Some(8));
    app.wait_for_watch(dir.path());

    // Overwrite the current image with a different-sized image. The watcher should evict +
    // re-decode it, and the displayed dimensions should update to the new size.
    let img = image::RgbaImage::from_pixel(20, 12, image::Rgba([200, 100, 50, 255]));
    img.save(dir.path().join("img-00.png")).unwrap();

    let state = app.wait_for_state(Duration::from_secs(8), |s| {
        s["image_width"].as_u64() == Some(20)
    });
    assert_eq!(
        state["image_width"].as_u64(),
        Some(20),
        "modifying the current image should re-decode it in place, got {state}"
    );
    assert_eq!(state["image_height"].as_u64(), Some(12));
    // Same file, same count — only the pixels changed.
    assert!(state["file"].as_str().unwrap().contains("img-00"));
    assert_eq!(state["total_files"].as_u64(), Some(3));
}

// ── Drag and drop ────────────────────────────────────────────────────────────────────────────
//
// A real drop is an OS drag session no HTTP request can synthesise, so these go through
// `POST /drop`, which hands the app the same path list winit delivers one `DroppedFile` at a
// time. What the paths then mean is `launch::classify_open_request`, unit-tested per platform;
// these prove the wiring from a drop to what's on screen.

#[test]
fn dropping_an_image_opens_it() {
    let dir = tempfile::tempdir().unwrap();
    let dropped = dir.path().join("dropped.png");
    create_white_image(&dropped, 24, 16);
    let Some(app) = SharedApp::start(&["DropToOpen"]) else {
        return;
    };

    app.post("/drop", dropped.to_str().unwrap());

    let state = app.wait_for_state(Duration::from_secs(5), |s| {
        s["file"].as_str().is_some_and(|f| f.contains("dropped"))
    });
    assert!(
        state["file"].as_str().unwrap().contains("dropped.png"),
        "the dropped image should be the one on screen, got {state}"
    );
    assert_eq!(state["image_width"].as_u64(), Some(24));
    assert_eq!(state["image_height"].as_u64(), Some(16));
}

#[test]
fn dropping_several_images_opens_the_first_and_lists_them_all() {
    // Dropped out of sorted order on purpose: the one that opens is the first the drop carried,
    // the way the command line opens the first file named, while the list it navigates is all
    // three in the user's sort order.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    let b = dir.path().join("b.png");
    let c = dir.path().join("c.png");
    create_white_image(&a, 8, 8);
    create_white_image(&b, 16, 16);
    create_white_image(&c, 8, 8);
    let Some(app) = SharedApp::start(&["DropToOpen"]) else {
        return;
    };

    let body = format!(
        "{}\n{}\n{}",
        b.to_str().unwrap(),
        a.to_str().unwrap(),
        c.to_str().unwrap()
    );
    app.post("/drop", &body);

    let state = app.wait_for_state(Duration::from_secs(5), |s| {
        s["total_files"].as_u64() == Some(3)
    });
    assert!(
        state["file"].as_str().unwrap().contains("b.png"),
        "the first image dropped is the one that opens, got {state}"
    );
    assert_eq!(state["total_files"].as_u64(), Some(3));
    assert_eq!(
        state["index"].as_u64(),
        Some(2),
        "b.png sorts second of three, and the position has to agree, got {state}"
    );
}

#[test]
fn dropping_a_folder_shows_that_folder() {
    // The platforms answer this differently by design, and both answers are right: macOS opens
    // browse mode at the folder, everywhere else its images become the image-mode list until M5
    // builds a browser there. Same fork a folder on the command line takes, so what's shared is
    // that the drop is answered at all — one of the two screens ends up showing the folder.
    // Which folder the macOS tree landed on is `dropping_a_folder_browses_it` in
    // `e2e_macos.rs`, where the harness can scope the tree's roots to keep the walk short.
    let (dir, _first) = create_multi_image_dir(3);
    let Some(app) = SharedApp::start(&["DropToOpen"]) else {
        return;
    };
    let folder = std::fs::canonicalize(dir.path()).unwrap();
    let folder_str = folder.to_string_lossy().into_owned();

    app.post("/drop", &folder_str);

    let answered = |s: &serde_json::Value| {
        s["view_mode"].as_str() == Some("browse")
            || s["file"]
                .as_str()
                .is_some_and(|f| f.starts_with(folder_str.as_str()))
    };
    let state = app.wait_for_state(Duration::from_secs(8), answered);
    assert!(
        answered(&state),
        "a dropped folder should be browsed or played, got {state}"
    );
}

#[test]
fn dropping_something_prvw_cant_open_leaves_the_image_alone() {
    // Opening it would put a decode error in the title bar and take the picture on screen away,
    // which is a worse answer than the drop simply not landing.
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, b"not an image").unwrap();
    let Some(app) = SharedApp::start(&["DropToOpen"]) else {
        return;
    };
    let before = app.get_state();

    app.post("/drop", notes.to_str().unwrap());
    std::thread::sleep(Duration::from_millis(300));

    let after = app.get_state();
    assert_eq!(
        before["file"].as_str(),
        after["file"].as_str(),
        "the image on screen should have survived the drop, got {after}"
    );
    assert_eq!(
        before["image_width"].as_u64(),
        after["image_width"].as_u64()
    );
}

// ── About ────────────────────────────────────────────────────────────────────────────────────

/// The About box opens and the app carries on running behind it.
///
/// The interesting assertion is the one `/show-about` makes on its own: the endpoint waits for
/// the event loop to acknowledge the command, so a box that opened a loop of its own would hold
/// the reply and fail here on the timeout. That's the exact shape of the Windows failure the
/// design warns about (a Win32 modal loop starves winit's pump, so `about_to_wait` stops running
/// and the slideshow freezes) and of the macOS one (an AppKit modal inside a winit callback
/// segfaults). Then a second command proves the pump is still turning afterwards.
#[test]
fn about_opens_without_holding_up_the_app() {
    let Some(app) = SharedApp::start(&["About"]) else {
        return;
    };

    app.post("/show-about", "");

    let state = app.post("/refresh", "");
    assert!(
        state["file"].as_str().is_some(),
        "the app should still answer with the open image while About is up"
    );
}

// ── Browse mode ──────────────────────────────────────────────────────────────────────────────
//
// Gated on `BrowseMode`, `BrowseFocus`, and `BrowseOpenSelected`. macOS drives an `NSOutlineView`
// plus an `NSCollectionView` and Windows a `SysTreeView32` plus a virtual `SysListView32`, and
// none of that shows up below: every assertion is about `/state`, which is the same contract on
// both. A platform that hasn't built a browser skips these rather than failing them, and the
// registries decide which is which.

// ── Browse mode: the mode switch ─────────────────────────────────────────────────────────────

#[test]
fn enter_browse_mode_with_enter_key() {
    let Some(app) = SharedApp::start(&["BrowseMode", "BrowseFocus", "BrowseOpenSelected"]) else {
        return;
    };
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
    let Some(app) = SharedApp::start(&["BrowseMode", "BrowseFocus", "BrowseOpenSelected"]) else {
        return;
    };
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
    let Some(app) = SharedApp::start(&["BrowseMode", "BrowseFocus", "BrowseOpenSelected"]) else {
        return;
    };
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
    app: &SharedApp,
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
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &images,
        home.path(),
    ) else {
        return;
    };

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
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &empty,
        home.path(),
    ) else {
        return;
    };

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
    // where the tree landed needs a scoped home to keep the reveal walk short and deterministic,
    // which is what the home override is for.
    let (home, images, _empty) = create_browse_home(4);
    let launch_image = images.join("img-00.png");
    let Some(app) = SharedApp::start_with_image_and_home(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &launch_image,
        home.path(),
    ) else {
        return;
    };
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
fn dropping_an_image_while_browsing_comes_back_to_the_viewer() {
    // An image has to be shown in image mode, so a drop while the browser is up brings the
    // viewer back. Without that the picture opens behind the browser and the drop looks ignored.
    let (home, images, _empty) = create_browse_home(3);
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &images,
        home.path(),
    ) else {
        return;
    };
    wait_for_browse_listed(&app, 3, Duration::from_secs(8));

    let dropped = home.path().join("dropped.png");
    let img = image::RgbaImage::from_pixel(24, 16, image::Rgba([90, 90, 90, 255]));
    img.save(&dropped).unwrap();
    app.post("/drop", dropped.to_str().unwrap());

    let state = app.wait_for_state(Duration::from_secs(5), |s| {
        s["view_mode"].as_str() == Some("image")
    });
    assert_eq!(
        state["view_mode"].as_str(),
        Some("image"),
        "a dropped image brings image mode back, got {state}"
    );
    assert!(
        state["file"].as_str().unwrap().ends_with("dropped.png"),
        "the dropped image is the one showing, got {state}"
    );
    assert_eq!(state["image_width"].as_u64(), Some(24));
}

#[test]
fn empty_folder_lists_zero_and_grid_stays_non_focusable() {
    // An empty folder → zero images, "(No images)", grid non-focusable: Tab stays on the tree.
    // The dir-arg launch is what makes the empty grid deterministic: entering browse from an
    // image always reveals a folder that has at least that image in it.
    let (home, _images, empty) = create_browse_home(2);
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &empty,
        home.path(),
    ) else {
        return;
    };

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
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &images,
        home.path(),
    ) else {
        return;
    };

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
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &images,
        home.path(),
    ) else {
        return;
    };
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
    let Some(app) = SharedApp::start_with_image_and_home(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &first,
        home.path(),
    ) else {
        return;
    };

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
    let Some(app) = SharedApp::start_with_image_and_home(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &first,
        home.path(),
    ) else {
        return;
    };

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
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &images,
        home.path(),
    ) else {
        return;
    };
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
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &images,
        home.path(),
    ) else {
        return;
    };
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
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &images,
        home.path(),
    ) else {
        return;
    };
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
    let Some(app) = SharedApp::start_browse_dir(
        &["BrowseMode", "BrowseFocus", "BrowseOpenSelected"],
        &images,
        home.path(),
    ) else {
        return;
    };
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
