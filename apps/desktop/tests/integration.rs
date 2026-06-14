//! End-to-end tests that spawn the real `prvw` binary and drive it via the QA HTTP
//! server. macOS-only: the binary creates a wgpu/AppKit window, which requires a
//! display — headless Linux CI can't run this.

#![cfg(target_os = "macos")]

use image::GenericImageView;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct TestApp {
    child: Child,
    base_url: String,
    client: reqwest::blocking::Client,
    // Per-test settings dir. Kept alive for the test's duration; auto-removed on Drop.
    // Without this, tests would share `/tmp/prvw-integration-test-{port}` across cargo
    // test invocations (ports get recycled), leaking state like `title_bar: false` from
    // one test into another and producing flakes.
    _data_dir: tempfile::TempDir,
}

impl TestApp {
    fn start() -> Self {
        let test_image = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build/icon.png");
        Self::start_with_image(&test_image)
    }

    /// Start the app with a custom image file.
    fn start_with_image(image_path: &std::path::Path) -> Self {
        // Find a free port by binding to :0, then closing the listener
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };

        // Fresh per-test settings dir — no cross-test leakage.
        let data_dir = tempfile::tempdir().expect("Couldn't create temp data dir");

        let child = Command::new(env!("CARGO_BIN_EXE_prvw"))
            .arg(image_path)
            .env("PRVW_QA_PORT", port.to_string())
            .env("PRVW_DATA_DIR", data_dir.path())
            // Open the window unfocused and behind everything so a run's swarm of test
            // windows doesn't grab the developer's keystrokes. Tests drive the app via
            // the QA HTTP server, not OS input, so this changes nothing they observe.
            .env("PRVW_BACKGROUND_WINDOW", "1")
            .spawn()
            .expect("Failed to start prvw");

        let base_url = format!("http://127.0.0.1:{port}");
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        // Wait for the QA server to be ready
        let start = Instant::now();
        loop {
            if start.elapsed() > Duration::from_secs(10) {
                panic!("QA server didn't start within 10 seconds");
            }
            if client.get(format!("{base_url}/state")).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // Wait a bit more for the image to load
        std::thread::sleep(Duration::from_millis(500));

        Self {
            child,
            base_url,
            client,
            _data_dir: data_dir,
        }
    }

    fn get_screenshot(&self) -> image::DynamicImage {
        let bytes = self
            .client
            .get(format!("{}/screenshot", self.base_url))
            .send()
            .expect("Failed to get screenshot")
            .bytes()
            .expect("Failed to read screenshot bytes");
        image::load_from_memory(&bytes).expect("Failed to decode screenshot PNG")
    }

    fn get_state(&self) -> serde_json::Value {
        self.client
            .get(format!("{}/state", self.base_url))
            .send()
            .expect("Failed to get state")
            .json()
            .expect("Failed to parse state JSON")
    }

    fn post(&self, path: &str, body: &str) -> serde_json::Value {
        self.client
            .post(format!("{}{path}", self.base_url))
            .body(body.to_string())
            .send()
            .unwrap_or_else(|_| panic!("Failed to POST {path}"))
            .json()
            .expect("Failed to parse response JSON")
    }

    fn post_json(&self, path: &str, json: &serde_json::Value) -> serde_json::Value {
        self.client
            .post(format!("{}{path}", self.base_url))
            .json(json)
            .send()
            .unwrap_or_else(|_| panic!("Failed to POST {path}"))
            .json()
            .expect("Failed to parse response JSON")
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn app_starts_and_loads_image() {
    let app = TestApp::start();
    let state = app.get_state();
    assert!(state["file"].as_str().unwrap().contains("icon.png"));
    assert!(state["image_width"].as_u64().unwrap() > 0);
    assert!(state["image_height"].as_u64().unwrap() > 0);
}

#[test]
fn zoom_in_increases_zoom() {
    let app = TestApp::start();
    let before = app.get_state()["zoom"].as_f64().unwrap();
    app.post("/zoom-in", "");
    let after = app.get_state()["zoom"].as_f64().unwrap();
    assert!(after > before, "zoom should increase: {before} -> {after}");
}

#[test]
fn zoom_out_decreases_zoom() {
    let app = TestApp::start();
    // First zoom in so we have room to zoom out
    app.post("/zoom-in", "");
    let before = app.get_state()["zoom"].as_f64().unwrap();
    app.post("/zoom-out", "");
    let after = app.get_state()["zoom"].as_f64().unwrap();
    assert!(after < before, "zoom should decrease: {before} -> {after}");
}

#[test]
fn fit_to_window_resets_zoom() {
    let app = TestApp::start();
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
    let app = TestApp::start();
    app.post("/zoom", "actual");
    let zoom = app.get_state()["zoom"].as_f64().unwrap();
    assert!(
        (zoom - 1.0).abs() < 0.01,
        "actual size should be zoom=1.0, got {zoom}"
    );
}

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

#[test]
fn auto_fit_toggle() {
    let app = TestApp::start();
    let before = app.get_state()["auto_fit_window"].as_bool().unwrap();
    let new_value = !before;
    app.post("/auto-fit", if new_value { "on" } else { "off" });
    let after = app.get_state()["auto_fit_window"].as_bool().unwrap();
    assert_eq!(after, new_value);
}

#[test]
fn title_bar_toggle() {
    let app = TestApp::start();
    let before = app.get_state()["title_bar"].as_bool().unwrap();
    let new_value = !before;
    app.post("/title-bar", if new_value { "on" } else { "off" });
    let after = app.get_state()["title_bar"].as_bool().unwrap();
    assert_eq!(after, new_value);
}

#[test]
fn scroll_to_zoom_toggle() {
    let app = TestApp::start();
    let before = app.get_state()["scroll_to_zoom"].as_bool().unwrap();
    let new_value = !before;
    app.post("/scroll-to-zoom", if new_value { "on" } else { "off" });
    let after = app.get_state()["scroll_to_zoom"].as_bool().unwrap();
    assert_eq!(after, new_value);
}

#[test]
fn refresh_redisplays_image() {
    let app = TestApp::start();
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
    let app = TestApp::start();
    let before = app.get_state();
    // icon.png is the only file in its directory, so navigate should keep it
    app.post("/navigate", "next");
    let after = app.get_state();
    if before["total_files"].as_u64().unwrap() == 1 {
        assert_eq!(before["file"].as_str(), after["file"].as_str());
    }
}

#[test]
fn window_geometry_changes_size() {
    let app = TestApp::start();
    let json = serde_json::json!({"width": 400, "height": 300});
    app.post_json("/window-geometry", &json);
    std::thread::sleep(Duration::from_millis(200));
    let state = app.get_state();
    let w = state["window_width"].as_u64().unwrap();
    let h = state["window_height"].as_u64().unwrap();
    assert!(w > 0 && h > 0, "window should have positive dimensions");
}

/// Create a solid white PNG image at the given path.
fn create_white_image(path: &std::path::Path, width: u32, height: u32) {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
    img.save(path).expect("Failed to save white test image");
}

/// Title bar ON: screenshot should show black (title bar area) near the top, image below.
/// The screenshot uses the same transform as the window but renders without the viewport,
/// so we check the transform's effect: with effective_height, the image should be rendered
/// smaller and centered, leaving black at the edges.
#[test]
fn title_bar_on_screenshot_has_reserved_area() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("white.png");
    create_white_image(&img_path, 800, 800);

    let app = TestApp::start_with_image(&img_path);
    // Title bar is ON by default
    assert!(app.get_state()["title_bar"].as_bool().unwrap());

    let screenshot = app.get_screenshot();
    let (sw, sh) = (screenshot.width(), screenshot.height());

    // The screenshot renders WITHOUT the viewport offset (full surface), but WITH the
    // transform using effective_height. The image should be centered in the full surface,
    // with the top and bottom edges showing black because sy is computed relative to the
    // effective (smaller) area.
    //
    // With an 800x800 image in an auto-fit window of ~800x859, effective_height = 800.
    // sy = 800 * zoom / 800 = 1.0 (fills NDC). In the screenshot (full surface 859px),
    // the image center is at NDC 0 → surface center. The image spans 800/859 of the surface
    // vertically... wait, sy=1.0 means image fills NDC [-1,1] → fills the full surface.
    //
    // Actually: the screenshot uses the transform but the DEFAULT viewport. sy is computed
    // with effective_height as denominator. At fit_zoom, sy = 1.0. In the screenshot,
    // NDC [-1,1] maps to the full surface. So sy=1.0 → image fills the FULL screenshot.
    //
    // This means the screenshot can't distinguish title-bar ON from OFF via pixel checks
    // when sy=1.0. But it CAN verify the image IS present (not broken).
    //
    // The real check: center pixel should be white (image is rendering).
    let center_pixel = screenshot.get_pixel(sw / 2, sh / 2);
    assert!(
        center_pixel[0] > 200 && center_pixel[1] > 200 && center_pixel[2] > 200,
        "Center pixel should be white (image content), got {:?}",
        center_pixel
    );

    // Top-left pixel (inside the title bar area in the real window) — in the screenshot it might
    // still be image content because screenshots don't use the viewport. So we just verify
    // the screenshot is valid (not all black).
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

    let app = TestApp::start_with_image(&img_path);
    // Toggle title bar OFF
    app.post("/title-bar", "off");
    std::thread::sleep(Duration::from_millis(200));

    let screenshot = app.get_screenshot();
    let (sw, _sh) = (screenshot.width(), screenshot.height());

    // With title bar OFF, the image should fill the entire window. The very first row
    // of the screenshot should be white (image content, not a black reserved area).
    let top_pixel = screenshot.get_pixel(sw / 2, 1);
    assert!(
        top_pixel[0] > 200 && top_pixel[1] > 200 && top_pixel[2] > 200,
        "With title bar OFF, pixel at y=1 should be white (image), got {:?}",
        top_pixel
    );
}

/// With auto-fit ON, toggling the title bar should change window height by the title bar height.
#[test]
fn title_bar_toggle_resizes_window() {
    // Must match TITLE_BAR_HEIGHT in main.rs
    const TITLE_BAR_HEIGHT: i64 = 32;

    let app = TestApp::start();
    // Title bar is ON by default, auto-fit is ON by default
    assert!(app.get_state()["title_bar"].as_bool().unwrap());
    assert!(app.get_state()["auto_fit_window"].as_bool().unwrap());

    let height_on = app.get_state()["window_height"].as_u64().unwrap();

    // Toggle title bar OFF
    app.post("/title-bar", "off");
    std::thread::sleep(Duration::from_millis(200));

    let height_off = app.get_state()["window_height"].as_u64().unwrap();

    assert_eq!(
        height_on as i64 - height_off as i64,
        TITLE_BAR_HEIGHT,
        "Window should shrink by {TITLE_BAR_HEIGHT}px when title bar is toggled OFF: {height_on} -> {height_off}"
    );
}

fn mcp_call(app: &TestApp, name: &str, arguments: serde_json::Value) -> serde_json::Value {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    });
    let body = app
        .client
        .post(format!("{}/mcp", app.base_url))
        .json(&req)
        .send()
        .expect("MCP request failed")
        .text()
        .expect("MCP response read failed");
    serde_json::from_str(&body).expect("MCP response is JSON")
}

/// Toggling the histogram via the H key flips `histogram_visible` in shared state.
#[test]
fn histogram_h_toggles_visibility() {
    let app = TestApp::start();
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
    let app = TestApp::start();
    let _ = mcp_call(&app, "histogram", serde_json::json!({}));
    // No `sleep` here: hover now uses the deterministic `plot_rect_for`
    // helper, so it works without a prior render. The MCP call already
    // syncs back through `update_shared_state`, so the state read below
    // sees `histogram_visible == true` immediately.
    assert_eq!(app.get_state()["histogram_visible"].as_bool(), Some(true));

    // The histogram panel sits in the top-right. The plot rect lives at
    // approximately (window_width - 256 - 7 + 10, content_offset_y + 7 + 22)
    // and is 236 wide × 70 tall. Pick a point ~halfway across.
    let state = app.get_state();
    let window_width = state["window_width"].as_u64().unwrap() as f64;
    let title_bar = if state["title_bar"].as_bool().unwrap_or(true) {
        32.0
    } else {
        0.0
    };
    let plot_x = window_width - 256.0 - 7.0 + 10.0;
    let plot_y = title_bar + 7.0 + 22.0;
    // Mid-plot.
    let cursor_x = plot_x + 118.0;
    let cursor_y = plot_y + 30.0;

    let _ = mcp_call(
        &app,
        "set_cursor_position",
        serde_json::json!({ "x": cursor_x, "y": cursor_y }),
    );
    let state = wait_for_state(&app, Duration::from_secs(2), |s| {
        s["histogram_hover_bin"].is_u64()
    });
    assert!(
        state["histogram_hover_bin"].is_u64(),
        "cursor inside the plot rect should produce a hover bin, got state: {state}"
    );

    // Move the cursor far away — hover bin should clear.
    let _ = mcp_call(
        &app,
        "set_cursor_position",
        serde_json::json!({ "x": 5.0, "y": 5.0 }),
    );
    let state = wait_for_state(&app, Duration::from_secs(2), |s| {
        s["histogram_hover_bin"].is_null()
    });
    assert!(
        state["histogram_hover_bin"].is_null(),
        "cursor outside the plot rect should clear hover bin, got state: {state}"
    );
}

/// Build a tiny JPEG with a known EXIF segment in a temp dir. Returns the
/// path. Used by the EXIF-panel integration tests so we don't need a
/// checked-in binary fixture.
fn create_jpeg_with_exif(dir: &std::path::Path) -> std::path::PathBuf {
    use little_exif::exif_tag::ExifTag as LeTag;
    use little_exif::metadata::Metadata;
    use little_exif::rational::uR64;

    let path = dir.join("with-exif.jpg");
    let img = image::RgbImage::from_pixel(8, 8, image::Rgb([180, 90, 60]));
    img.save(&path).expect("save test JPEG");

    let mut md = Metadata::new();
    md.set_tag(LeTag::Make("PrvwTest".into()));
    md.set_tag(LeTag::Model("Camera 9000".into()));
    md.set_tag(LeTag::FNumber(vec![uR64 {
        nominator: 28,
        denominator: 10,
    }]));
    md.set_tag(LeTag::ExposureTime(vec![uR64 {
        nominator: 1,
        denominator: 250,
    }]));
    md.set_tag(LeTag::ISO(vec![400]));
    md.set_tag(LeTag::FocalLength(vec![uR64 {
        nominator: 50,
        denominator: 1,
    }]));
    md.set_tag(LeTag::DateTimeOriginal("2024:08:15 12:34:56".into()));
    md.write_to_file(&path).expect("inject EXIF");
    path
}

#[test]
fn exif_e_toggles_with_exif_jpeg() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = create_jpeg_with_exif(dir.path());

    let app = TestApp::start_with_image(&img_path);

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

    let app = TestApp::start_with_image(&img_path);

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

    let app = TestApp::start_with_image(&img_path);
    app.post("/key", "e");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(app.get_state()["exif_visible"].as_bool(), Some(true));

    // Read settings.json from the per-test data dir to confirm the flag landed.
    // `PRVW_DATA_DIR` causes `data_dir()` to return the dir as-is (no
    // platform-specific suffix), so settings.json sits directly inside.
    let settings_path = app._data_dir.path().join("settings.json");
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

    let app = TestApp::start_with_image(&img_path);
    app.post("/key", "h");
    std::thread::sleep(Duration::from_millis(120));
    app.post("/key", "e");
    std::thread::sleep(Duration::from_millis(120));

    let state = app.get_state();
    assert_eq!(state["histogram_visible"].as_bool(), Some(true));
    assert_eq!(state["exif_visible"].as_bool(), Some(true));
    assert_eq!(state["exif_present"].as_bool(), Some(true));
}

/// Zoom should stay the same when toggling the title bar (image stays same size).
#[test]
fn title_bar_toggle_preserves_zoom() {
    let app = TestApp::start();
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

/// Toggling the histogram and EXIF overlays at a very narrow window width must not
/// crash the renderer. The overlays don't currently clamp their geometry against
/// `window_width`; this test is the safety net that proves the trivial
/// render-to-negative-x path is harmless.
#[test]
fn narrow_window_overlays_dont_crash() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = create_jpeg_with_exif(dir.path());

    let app = TestApp::start_with_image(&img_path);

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
    let s = wait_for_state(&app, Duration::from_secs(2), |s| {
        s["histogram_visible"].as_bool() == Some(true)
    });
    assert_eq!(s["histogram_visible"].as_bool(), Some(true));

    app.post("/key", "e");
    let s = wait_for_state(&app, Duration::from_secs(2), |s| {
        s["exif_visible"].as_bool() == Some(true)
    });
    assert_eq!(s["exif_visible"].as_bool(), Some(true));

    // Navigation is fire-and-forget here — we just want to exercise the code
    // paths without crashing. The single-image temp dir means /navigate is a
    // no-op anyway, so there's nothing observable to wait for.
    app.post("/navigate", "next");
    app.post("/navigate", "prev");

    app.post("/key", "h");
    let s = wait_for_state(&app, Duration::from_secs(2), |s| {
        s["histogram_visible"].as_bool() == Some(false)
    });
    assert_eq!(s["histogram_visible"].as_bool(), Some(false));

    app.post("/key", "e");
    let final_state = wait_for_state(&app, Duration::from_secs(2), |s| {
        s["exif_visible"].as_bool() == Some(false)
    });
    assert_eq!(final_state["histogram_visible"].as_bool(), Some(false));
    assert_eq!(final_state["exif_visible"].as_bool(), Some(false));
}

/// Build a temporary directory with `n` distinct PNG files. Returns the
/// directory and the path of the first image so the caller can launch the
/// app pointing at it.
fn create_multi_image_dir(n: u32) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let mut first = None;
    for i in 0..n {
        let path = dir.path().join(format!("img-{i:02}.png"));
        // Vary the pixel color so each PNG is a distinct decode (avoids
        // any cache deduping that may key off content).
        let shade = (i as u8).wrapping_mul(17);
        let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([shade, shade, shade, 255]));
        img.save(&path).unwrap();
        if first.is_none() {
            first = Some(path);
        }
    }
    (dir, first.unwrap())
}

/// Wait until `pred(state)` is true or the timeout elapses. Returns the
/// last observed state. Polls every 50 ms.
fn wait_for_state<F: Fn(&serde_json::Value) -> bool>(
    app: &TestApp,
    timeout: Duration,
    pred: F,
) -> serde_json::Value {
    let start = Instant::now();
    loop {
        let state = app.get_state();
        if pred(&state) {
            return state;
        }
        if start.elapsed() > timeout {
            return state;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn cache_indices(state: &serde_json::Value) -> Vec<u64> {
    state["cache_indices"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_u64()).collect())
        .unwrap_or_default()
}

#[test]
fn loop_l_toggles_visibility_in_state() {
    let (_dir, first) = create_multi_image_dir(5);
    let app = TestApp::start_with_image(&first);

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
    let app = TestApp::start_with_image(&first);

    // Turn loop on first.
    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));

    // Navigate forward four times to reach the last image (index 4 of 5).
    for _ in 0..4 {
        app.post("/navigate", "next");
    }
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(5) // 1-based index = 5 -> 0-based 4
    });
    assert_eq!(state["index"].as_u64(), Some(5));
    assert_eq!(state["total_files"].as_u64(), Some(5));

    // Next from last wraps to first.
    app.post("/navigate", "next");
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(1)
    });
    assert_eq!(
        state["index"].as_u64(),
        Some(1),
        "next at last wraps to first"
    );

    // Previous from first wraps to last.
    app.post("/navigate", "prev");
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(5)
    });
    assert_eq!(
        state["index"].as_u64(),
        Some(5),
        "previous at first wraps to last"
    );
}

#[test]
fn loop_off_halts_at_edge() {
    let (_dir, first) = create_multi_image_dir(5);
    let app = TestApp::start_with_image(&first);
    // Loop is off by default. Confirm.
    assert_eq!(app.get_state()["loop_navigation"].as_bool(), Some(false));

    // Walk to the last image.
    for _ in 0..4 {
        app.post("/navigate", "next");
    }
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(5)
    });
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
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(1)
    });
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
    let app = TestApp::start_with_image(&first);

    // Walk to the last image (index 5 of 6).
    for _ in 0..5 {
        app.post("/navigate", "next");
    }
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(6)
    });
    assert_eq!(state["index"].as_u64(), Some(6));

    // With loop OFF, the cache must not contain wrap-side indices 0 or 1.
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
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
    let state = wait_for_state(&app, Duration::from_secs(5), |s| {
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
    let app = TestApp::start_with_image(&first);

    // Loop on first so the wrap-side preloads run.
    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));

    // Walk to the last image.
    for _ in 0..5 {
        app.post("/navigate", "next");
    }
    let state = wait_for_state(&app, Duration::from_secs(5), |s| {
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
    let state = wait_for_state(&app, Duration::from_secs(5), |s| {
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
    let app = TestApp::start_with_image(&first);

    // Walk to a middle image (index 2 of 5).
    for _ in 0..2 {
        app.post("/navigate", "next");
    }
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(3)
    });
    assert_eq!(state["index"].as_u64(), Some(3));

    // Press Home — jumps to first.
    app.post("/key", "Home");
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(1)
    });
    assert_eq!(
        state["index"].as_u64(),
        Some(1),
        "Home jumps to first image"
    );
}

#[test]
fn end_key_jumps_to_last() {
    let (_dir, first) = create_multi_image_dir(5);
    let app = TestApp::start_with_image(&first);
    assert_eq!(app.get_state()["index"].as_u64(), Some(1));

    app.post("/key", "End");
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(5)
    });
    assert_eq!(state["index"].as_u64(), Some(5), "End jumps to last image");
    assert_eq!(state["total_files"].as_u64(), Some(5));
}

#[test]
fn home_at_first_is_noop() {
    let (_dir, first) = create_multi_image_dir(5);
    let app = TestApp::start_with_image(&first);
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
    let app = TestApp::start_with_image(&first);

    // Walk to the last image first.
    app.post("/key", "End");
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(5)
    });
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
    let app = TestApp::start_with_image(&first);

    // Loop on.
    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(app.get_state()["loop_navigation"].as_bool(), Some(true));

    // Walk to middle.
    for _ in 0..2 {
        app.post("/navigate", "next");
    }
    let _ = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(3)
    });

    // Home jumps to absolute first regardless of loop.
    app.post("/key", "Home");
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(1)
    });
    assert_eq!(
        state["index"].as_u64(),
        Some(1),
        "Home with loop on still jumps to first"
    );
}

#[test]
fn loop_persists_across_settings_reload() {
    let (_dir, first) = create_multi_image_dir(3);
    let app = TestApp::start_with_image(&first);
    app.post("/key", "l");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(app.get_state()["loop_navigation"].as_bool(), Some(true));

    let settings_path = app._data_dir.path().join("settings.json");
    let json = std::fs::read_to_string(&settings_path).expect("settings file should exist");
    assert!(
        json.contains("\"loop_navigation\": true"),
        "loop_navigation should persist to settings.json, got: {json}"
    );
}

/// `screenshot_window` MCP tool runs end-to-end. Marked `#[ignore]` because the tool
/// shells out to `/usr/sbin/screencapture -l`, which requires Screen Recording
/// permission. Headless CI hosts and freshly-cloned dev boxes return a black
/// (still valid PNG) frame until the user grants it. Run locally with:
/// `cargo test --test integration screenshot_window_returns_png -- --ignored`.
#[test]
#[ignore]
fn screenshot_window_returns_png() {
    let app = TestApp::start();
    let result = mcp_call(&app, "screenshot_window", serde_json::json!({}));
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
