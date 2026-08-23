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

/// With auto-fit ON, toggling the title bar should change window height by the title bar height.
#[test]
fn title_bar_toggle_resizes_window() {
    let Some(app) = SharedApp::start(&["TitleBar", "AutoFitWindow"]) else {
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
    let Some(app) = SharedApp::start(&["TitleBar"]) else {
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
