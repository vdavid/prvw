//! `TestApp`: one spawned `prvw` process plus the HTTP client that drives it.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::fixtures::create_fixture_image;

pub struct TestApp {
    child: Child,
    pub base_url: String,
    pub client: reqwest::blocking::Client,
    // Per-test settings dir. Kept alive for the test's duration; auto-removed on Drop.
    // Without this, tests would share `prvw-integration-test-{port}` across cargo test
    // invocations (ports get recycled), leaking state like `title_bar: false` from one test
    // into another and producing flakes.
    data_dir: tempfile::TempDir,
    // Temp home holding the generated default fixture, for `TestApp::start`. `None` when the
    // test supplied its own image. Kept alive for the test's duration.
    _fixture_home: Option<tempfile::TempDir>,
}

impl TestApp {
    /// Start the app on the default fixture: a freshly generated image, alone in its own
    /// folder, inside a fresh temp home.
    ///
    /// Generating it keeps the suite self-contained (no checked-in blob, no path outside the
    /// repo's control) and makes two things the tests already assume true by construction
    /// rather than by luck: the fixture is the only image in its directory (so the
    /// single-file navigation test really sees one file), and it sits one level under the home
    /// dir (so browse mode's tree reveal is a short, deterministic walk instead of a descent
    /// from the real home folder).
    pub fn start() -> Self {
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
    pub fn start_with_image(image_path: &std::path::Path) -> Self {
        Self::start_with_arg_and_home(image_path, None)
    }

    /// Start the app pointing at a directory (dir-arg launch → browse mode), with the home dir
    /// set to `home` so the browse tree's home root contains the directory and the reveal walk
    /// is a short, deterministic chain. Used by the browse integration tests.
    pub fn start_browse_dir(dir: &std::path::Path, home: &std::path::Path) -> Self {
        Self::start_with_arg_and_home(dir, Some(home))
    }

    /// Start the app with a single CLI argument (a file or directory) and an optional home
    /// override. The home override scopes the browse tree's home root so reveal walks are short
    /// and deterministic (the target sits directly under home).
    pub fn start_with_arg_and_home(arg: &std::path::Path, home: Option<&std::path::Path>) -> Self {
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
            // Canonicalize the home path so it matches the launch arg's canonical form. On macOS
            // `$TMPDIR` lives under `/var/folders/...`, a symlink to `/private/var/...`;
            // `main.rs` canonicalizes the launch path, so an un-canonicalized home wouldn't
            // string-prefix-match it and the tree's reveal walk would pick the `/` root (a deep,
            // slow walk) instead of the home root. Canonicalizing both makes the home root the
            // longest-prefix match.
            let canonical_home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
            // `HOME` is what the browse tree reads on macOS; `USERPROFILE` is the Windows
            // spelling of the same idea. Setting both keeps the fixture scoped on whichever
            // host runs the suite, rather than silently falling back to the real home folder.
            command.env("HOME", &canonical_home);
            command.env("USERPROFILE", &canonical_home);
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
            data_dir,
            _fixture_home: None,
        }
    }

    /// The per-test `settings.json`. `PRVW_DATA_DIR` makes `settings::persistence::data_dir()`
    /// return the dir as-is (no platform-specific suffix), so the file sits directly inside.
    pub fn settings_path(&self) -> std::path::PathBuf {
        self.data_dir.path().join("settings.json")
    }

    pub fn get_screenshot(&self) -> image::DynamicImage {
        let bytes = self
            .client
            .get(format!("{}/screenshot", self.base_url))
            .send()
            .expect("Failed to get screenshot")
            .bytes()
            .expect("Failed to read screenshot bytes");
        image::load_from_memory(&bytes).expect("Failed to decode screenshot PNG")
    }

    pub fn get_state(&self) -> serde_json::Value {
        self.client
            .get(format!("{}/state", self.base_url))
            .send()
            .expect("Failed to get state")
            .json()
            .expect("Failed to parse state JSON")
    }

    /// The parity table the app renders from the layer-1 registries (`src/parity/`). Same
    /// answer on every host, because the registries carry no `#[cfg]`.
    pub fn get_parity(&self) -> serde_json::Value {
        self.client
            .get(format!("{}/parity", self.base_url))
            .send()
            .expect("Failed to get parity table")
            .json()
            .expect("Failed to parse parity JSON")
    }

    /// Wait until `pred(state)` is true or the timeout elapses. Returns the last observed
    /// state. Polls every 50 ms.
    pub fn wait_for_state<F: Fn(&serde_json::Value) -> bool>(
        &self,
        timeout: Duration,
        pred: F,
    ) -> serde_json::Value {
        let start = Instant::now();
        loop {
            let state = self.get_state();
            if pred(&state) {
                return state;
            }
            if start.elapsed() > timeout {
                return state;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Block until the app's live-sync watch on `folder` is armed, then return.
    ///
    /// `/state` answers well before the folder watcher exists, let alone before its filesystem
    /// event stream starts, and those streams report only what happens after they start. A test
    /// that mutates the folder in that window gets no event at all and then waits out its full
    /// timeout, which was the single biggest source of flakes in this suite and gets worse the
    /// busier the machine is. `watched_folders` reports what the watcher thread has actually
    /// applied, so polling it closes the race instead of sleeping at it.
    pub fn wait_for_watch(&self, folder: &std::path::Path) {
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

    pub fn post(&self, path: &str, body: &str) -> serde_json::Value {
        self.client
            .post(format!("{}{path}", self.base_url))
            .body(body.to_string())
            .send()
            .unwrap_or_else(|_| panic!("Failed to POST {path}"))
            .json()
            .expect("Failed to parse response JSON")
    }

    pub fn post_json(&self, path: &str, json: &serde_json::Value) -> serde_json::Value {
        self.client
            .post(format!("{}{path}", self.base_url))
            .json(json)
            .send()
            .unwrap_or_else(|_| panic!("Failed to POST {path}"))
            .json()
            .expect("Failed to parse response JSON")
    }

    /// Call an MCP tool over `POST /mcp` and return the JSON-RPC response.
    pub fn mcp_call(&self, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let body = self
            .client
            .post(format!("{}/mcp", self.base_url))
            .json(&req)
            .send()
            .expect("MCP request failed")
            .text()
            .expect("MCP response read failed");
        serde_json::from_str(&body).expect("MCP response is JSON")
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
