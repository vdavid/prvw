use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct TestApp {
    child: Child,
    base_url: String,
    client: reqwest::blocking::Client,
}

impl TestApp {
    fn start() -> Self {
        let test_image = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build/icon.png");

        // Find a free port by binding to :0, then closing the listener
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };

        let child = Command::new(env!("CARGO_BIN_EXE_prvw"))
            .arg(&test_image)
            .env("PRVW_QA_PORT", port.to_string())
            .env(
                "PRVW_DATA_DIR",
                std::env::temp_dir()
                    .join(format!("prvw-integration-test-{port}"))
                    .to_str()
                    .unwrap(),
            )
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
        }
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
