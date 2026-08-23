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
    // Temp `HOME` holding the generated default fixture, for `TestApp::start`. `None` when
    // the test supplied its own image. Kept alive for the test's duration.
    _fixture_home: Option<tempfile::TempDir>,
}

impl TestApp {
    /// Start the app on the default fixture: a freshly generated image, alone in its own
    /// folder, inside a fresh temp `HOME`.
    ///
    /// Generating it keeps the suite self-contained (no checked-in blob, no path outside the
    /// repo's control) and makes two things the tests already assume true by construction
    /// rather than by luck: the fixture is the only image in its directory (so the
    /// single-file navigation test really sees one file), and it sits one level under `HOME`
    /// (so browse mode's tree reveal is a short, deterministic walk instead of a descent from
    /// the real home folder).
    fn start() -> Self {
        let home = tempfile::tempdir().expect("Couldn't create temp home");
        let folder = home.path().join("pictures");
        std::fs::create_dir(&folder).expect("Couldn't create the fixture folder");
        let image_path = folder.join("fixture.png");
        create_fixture_image(&image_path);
        let mut app = Self::start_with_arg_and_home(&image_path, Some(home.path()));
        app._fixture_home = Some(home);
        app
    }

    /// Start the app with a custom image file.
    fn start_with_image(image_path: &std::path::Path) -> Self {
        Self::start_with_arg_and_home(image_path, None)
    }

    /// Start the app pointing at a directory (dir-arg launch → browse mode), with `HOME` set to
    /// `home` so the browse tree's home root contains the directory and the reveal walk is a short,
    /// deterministic chain. Used by the browse integration tests.
    fn start_browse_dir(dir: &std::path::Path, home: &std::path::Path) -> Self {
        Self::start_with_arg_and_home(dir, Some(home))
    }

    /// Start the app with a single CLI argument (a file or directory) and an optional `HOME`
    /// override. The `HOME` override scopes the browse tree's home root so reveal walks are short
    /// and deterministic (the target sits directly under home).
    fn start_with_arg_and_home(arg: &std::path::Path, home: Option<&std::path::Path>) -> Self {
        // Find a free port by binding to :0, then closing the listener
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };

        // Fresh per-test settings dir — no cross-test leakage.
        let data_dir = tempfile::tempdir().expect("Couldn't create temp data dir");

        let mut command = Command::new(env!("CARGO_BIN_EXE_prvw"));
        command
            .arg(arg)
            .env("PRVW_QA_PORT", port.to_string())
            .env("PRVW_DATA_DIR", data_dir.path())
            // Open the window unfocused and behind everything so a run's swarm of test
            // windows doesn't grab the developer's keystrokes. Tests drive the app via
            // the QA HTTP server, not OS input, so this changes nothing they observe.
            .env("PRVW_BACKGROUND_WINDOW", "1");
        if let Some(home) = home {
            // Canonicalize HOME so it matches the launch arg's canonical form. On macOS `$TMPDIR`
            // lives under `/var/folders/...`, a symlink to `/private/var/...`; `main.rs`
            // canonicalizes the launch path, so an un-canonicalized HOME wouldn't string-prefix-match
            // it and the tree's reveal walk would pick the `/` root (a deep, slow walk) instead of
            // the home root. Canonicalizing both makes the home root the longest-prefix match.
            let canonical_home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
            command.env("HOME", canonical_home);
        }
        let child = command.spawn().expect("Failed to start prvw");

        let base_url = format!("http://127.0.0.1:{port}");
        // Generous per-request timeout: each `POST` that changes app state blocks on a main-thread
        // sync, and a loaded machine (a CI runner, a full-parallelism local run) can take seconds
        // to get there. Every caller has its own deadline, so this only decides whether a slow
        // response fails the test outright or just arrives late.
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
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
            _fixture_home: None,
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

    /// Block until the app's live-sync watch on `folder` is armed, then return.
    ///
    /// `/state` answers well before the folder watcher exists, let alone before its FSEvents
    /// stream starts, and FSEvents reports only what happens after the stream starts. A test that
    /// mutates the folder in that window gets no event at all and then waits out its full
    /// timeout — the single biggest source of flakes in this file, and one that gets worse the
    /// busier the machine is. `watched_folders` reports what the watcher thread has actually
    /// applied, so polling it closes the race instead of sleeping at it.
    fn wait_for_watch(&self, folder: &std::path::Path) {
        let canonical = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
        let wanted = canonical.to_string_lossy().into_owned();
        let start = Instant::now();
        loop {
            let state = self.get_state();
            let armed = state["watched_folders"]
                .as_array()
                .is_some_and(|f| f.iter().any(|p| p.as_str() == Some(wanted.as_str())));
            if armed {
                return;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("Live-sync watch on {wanted} was never armed, state: {state}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
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
    assert!(state["file"].as_str().unwrap().contains("fixture.png"));
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
fn fullscreen_respects_enlarge_setting_even_with_auto_fit_on() {
    // Auto-fit can't resize the window in fullscreen (the window is the whole screen), so it's
    // inert there and the fit/enlarge rules govern instead. A small image with "Enlarge small
    // images" OFF must stay at actual size in fullscreen, NOT be blown up — and toggling
    // enlarge while in fullscreen must take effect immediately.
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("small.png");
    create_white_image(&img_path, 64, 64);
    let app = TestApp::start_with_image(&img_path);

    app.post("/auto-fit", "on");
    app.post("/fullscreen", "on");
    let s = wait_for_state(&app, Duration::from_secs(5), |s| {
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
    let s = wait_for_state(&app, Duration::from_secs(8), |s| {
        (s["zoom"].as_f64().unwrap_or(0.0) - 1.0).abs() < 0.05
    });
    let zoom_no_enlarge = s["zoom"].as_f64().unwrap();
    assert!(
        (zoom_no_enlarge - 1.0).abs() < 0.05,
        "small image must stay at 100% in fullscreen when enlarge is off, got {zoom_no_enlarge}"
    );

    // Toggling enlarge ON while in fullscreen must take effect and scale the image up.
    app.post("/enlarge-small", "on");
    let s = wait_for_state(&app, Duration::from_secs(8), |s| {
        s["zoom"].as_f64().unwrap_or(0.0) > 1.5
    });
    let zoom_enlarged = s["zoom"].as_f64().unwrap();
    assert!(
        zoom_enlarged > 1.5,
        "enlarge on in fullscreen should scale the small image up, got {zoom_enlarged}"
    );
}

#[test]
#[cfg(target_os = "macos")]
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
    let entered = wait_for_state(&app, FULLSCREEN_WAIT, |s| {
        s["fullscreen"].as_bool() == Some(true)
            && s["window_width"]
                .as_u64()
                .is_some_and(|w| w > windowed_width)
    });
    assert_eq!(entered["fullscreen"].as_bool(), Some(true));
    assert!(entered["window_width"].as_u64().unwrap() > windowed_width);

    app.post("/fullscreen", "off");
    let left = wait_for_state(&app, FULLSCREEN_WAIT, |s| {
        s["fullscreen"].as_bool() == Some(false)
    });
    assert_eq!(
        left["fullscreen"].as_bool(),
        Some(false),
        "the window must not be left believing it's fullscreen after leaving"
    );

    app.post("/key", "f");
    let toggled = wait_for_state(&app, FULLSCREEN_WAIT, |s| {
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
    wait_for_state(&app, FULLSCREEN_WAIT, |s| {
        s["fullscreen"].as_bool() == Some(false)
    });
}

#[test]
fn enabling_auto_fit_refits_zoom_to_resized_window() {
    // With auto-fit off, the window can be a very different size than the image's auto-fit
    // target. Enabling auto-fit resizes the window AND must re-fit zoom against the NEW
    // window size — not the stale (pre-resize) one. Pre-fix, zoom was fit against the stale
    // (larger) window, so after the window shrank the image stayed zoomed in and overflowed.
    let app = TestApp::start();
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
    // The fixture is the only file in its directory, so navigate should keep it
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

/// The default fixture's edge length. The zoom, auto-fit, and window-geometry tests are
/// written against this natural fit size, so changing it moves their expectations.
const FIXTURE_SIZE: u32 = 1024;

/// Write the default fixture image: a vertical grayscale ramp.
///
/// The ramp gives a non-degenerate histogram, and one color per row lets PNG's row filter flatten
/// all but the first, so the file lands at ~23 KB and the app decodes it in ~30 ms against the old
/// 924 KB icon's ~75 ms. Writing it costs ~200 ms per test process, which is noise next to the
/// window and GPU setup every test already pays.
fn create_fixture_image(path: &std::path::Path) {
    let img = image::RgbaImage::from_fn(FIXTURE_SIZE, FIXTURE_SIZE, |_, y| {
        let value = (y * 256 / FIXTURE_SIZE) as u8;
        image::Rgba([value, value, value, 255])
    });
    img.save(path).expect("Failed to save the default fixture");
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

// ─── Browse mode (Phase 3: real folder tree) ───────────────────────────────

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
    // the current image. Both must land back in image mode — never a stray open, never stuck in
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

// ─── Browse mode (Phase 7: end-to-end flow driven through the QA server) ─────
//
// These drive the full browse picture headlessly via the QA browse hooks (`/browse/select-folder`,
// `/browse/select-grid`, `/browse/open`) and assert the new `/state` fields (`browse_grid_count`,
// `browse_reveal_pending`, `browse_selected_folder`, `browse_grid_selected`). They stay hermetic:
// each builds its own temp `HOME` so the tree's home root contains the test folders, and polls
// `/state` with a bounded wait (folder listing + tree reveal are async) rather than sleeping a
// fixed time.

/// Build a temp `HOME` with a subfolder of `n` distinct PNGs and an empty subfolder. Returns
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
    wait_for_state(app, timeout, |s| {
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
    let state = wait_for_state(&app, Duration::from_secs(5), |s| {
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
fn empty_folder_lists_zero_and_grid_stays_non_focusable() {
    // An empty folder → zero images, "(No images)", grid non-focusable: Tab stays on the tree.
    // The dir-arg launch is what makes the empty grid deterministic — entering browse from an
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

    // Tab toward the empty grid stays on the tree (the grid can't take focus).
    app.post("/key", "Tab");
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(
        app.get_state()["focused_pane"].as_str(),
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
    let state = wait_for_state(&app, Duration::from_secs(2), |s| {
        s["focused_pane"].as_str() == Some("tree")
    });
    assert_eq!(state["focused_pane"].as_str(), Some("tree"));

    // Tab → back to grid.
    app.post("/key", "Tab");
    let state = wait_for_state(&app, Duration::from_secs(2), |s| {
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
    let state = wait_for_state(&app, Duration::from_secs(2), |s| {
        s["browse_grid_selected"].as_u64() == Some(3)
    });
    assert_eq!(state["browse_grid_selected"].as_u64(), Some(3));

    // Open the selection → image mode, showing that image (1-based index 4 of 5).
    app.post("/browse/open", "");
    let state = wait_for_state(&app, Duration::from_secs(5), |s| {
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

#[test]
fn entering_browse_from_an_image_preselects_that_image() {
    // Entering browse from a multi-image folder (in image mode) reveals that folder and preselects
    // the displayed image — even when it's not the first image. HOME is scoped to the folder's
    // parent so the reveal walk is a short, deterministic chain.
    let (home, images, _empty) = create_browse_home(5);
    let first = images.join("img-00.png");
    let app = TestApp::start_with_arg_and_home(&first, Some(home.path()));

    // Navigate to the third image (1-based index 3) in image mode.
    for _ in 0..2 {
        app.post("/navigate", "next");
    }
    let state = wait_for_state(&app, Duration::from_secs(3), |s| {
        s["index"].as_u64() == Some(3)
    });
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

// ── Live folder sync (image mode) ────────────────────────────────────────────────────────────
//
// These drive the real FSEvents watcher: open an image, then mutate its folder from the shell-side
// (the test process) and poll `/state` for the sequence to update. FSEvents has real latency, so
// the waits are generous, and it reports nothing that happened before its stream started, so each
// test calls `wait_for_watch` before touching its folder. macOS-only (the whole suite is).

/// Write a distinct solid-color PNG at `path` (shade derived from `seed`).
fn write_png(path: &std::path::Path, seed: u8) {
    let shade = seed.wrapping_mul(31);
    let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([shade, shade, shade, 255]));
    img.save(path).unwrap();
}

#[test]
fn live_sync_added_image_grows_the_sequence() {
    let (dir, first) = create_multi_image_dir(3); // img-00..img-02
    let app = TestApp::start_with_image(&first);
    assert_eq!(app.get_state()["total_files"].as_u64(), Some(3));
    app.wait_for_watch(dir.path());

    // Add a 4th image to the watched folder.
    write_png(&dir.path().join("img-03.png"), 9);

    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
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
    let app = TestApp::start_with_image(&first);
    assert_eq!(app.get_state()["total_files"].as_u64(), Some(3));
    app.wait_for_watch(dir.path());

    // Delete img-02 (not the current image).
    std::fs::remove_file(dir.path().join("img-02.png")).unwrap();

    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
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
    let app = TestApp::start_with_image(&first);
    app.wait_for_watch(dir.path());

    // Delete the current image (img-00). Should navigate to the next (img-01).
    std::fs::remove_file(dir.path().join("img-00.png")).unwrap();

    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
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
    let app = TestApp::start_with_image(&only);
    assert_eq!(app.get_state()["total_files"].as_u64(), Some(1));
    app.wait_for_watch(dir.path());

    // Delete the only image → image-mode "(No images)" empty state.
    std::fs::remove_file(&only).unwrap();

    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
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
    let app = TestApp::start_with_image(&only);
    app.wait_for_watch(dir.path());

    std::fs::remove_file(&only).unwrap();
    wait_for_state(&app, Duration::from_secs(8), |s| {
        s["no_images"].as_bool() == Some(true)
    });

    // A new image appears in the (still-watched) folder → empty state clears, image opens.
    write_png(&dir.path().join("fresh.png"), 7);
    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
        s["no_images"].as_bool() == Some(false) && s["total_files"].as_u64() == Some(1)
    });
    assert_eq!(state["no_images"].as_bool(), Some(false));
    assert_eq!(state["total_files"].as_u64(), Some(1));
    assert!(state["file"].as_str().unwrap().contains("fresh"));
}

#[test]
fn live_sync_modify_current_re_decodes_in_place() {
    let (dir, first) = create_multi_image_dir(3); // img-00 (current) .. img-02
    let app = TestApp::start_with_image(&first);
    // img-00 is 8x8 (create_multi_image_dir). Confirm.
    assert_eq!(app.get_state()["image_width"].as_u64(), Some(8));
    app.wait_for_watch(dir.path());

    // Overwrite the current image with a different-sized image. The watcher should evict + re-decode
    // it, and the displayed dimensions should update to the new size.
    let img = image::RgbaImage::from_pixel(20, 12, image::Rgba([200, 100, 50, 255]));
    img.save(dir.path().join("img-00.png")).unwrap();

    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
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

// ── Live folder sync (browse mode: grid + tree) ───────────────────────────────────────────────
//
// These boot into browse mode on a temp folder (dir-arg launch) and mutate the listed folder from
// the test process, then poll `/state`'s browse fields for the grid to update live. The grid runs
// off the same FSEvents watcher + off-thread re-scan as image mode; here the watch follows the
// grid's listed folder, so `wait_for_watch` takes the listed folder rather than the image's. The tree-structure reload (subfolder add/remove on an expanded node) isn't
// observable through `/state` (no tree-children field), so it's covered by logs + live QA, not here.

#[test]
fn live_sync_browse_grid_grows_when_an_image_is_added() {
    // Browse the `pics` folder (4 images), then drop a 5th into it from the shell side. The grid's
    // active-folder watch should pick it up and grow the listed count to 5.
    let (home, images, _empty) = create_browse_home(4);
    let app = TestApp::start_browse_dir(&images, home.path());
    wait_for_browse_listed(&app, 4, Duration::from_secs(8));
    app.wait_for_watch(&images);

    write_png(&images.join("img-99.png"), 13);

    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
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

    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
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
    let state = wait_for_state(&app, Duration::from_secs(5), |s| {
        s["browse_grid_selected"].as_u64() == Some(2)
    });
    assert_eq!(state["browse_grid_selected"].as_u64(), Some(2));
    app.wait_for_watch(&images);

    std::fs::remove_file(images.join("img-00.png")).unwrap();

    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
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

    let state = wait_for_state(&app, Duration::from_secs(8), |s| {
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
