//! `TestApp`: one spawned `prvw` process plus the HTTP client that drives it.
//!
//! ## Every failed request says what happened to the app
//!
//! A test that can't reach the QA server has no other way to find out why, and on Windows there
//! is no terminal to look at afterwards: `prvw.exe` is a GUI-subsystem binary, and a CI run is
//! over by the time anyone reads it. So the harness pipes the child's stderr into
//! [`AppLog`], and every request that fails panics with [`TestApp::diagnose`] — which names the
//! request, what the transport actually did (a timed-out request means the app is wedged; a
//! reset connection means it's gone), whether the process is still alive, its exit status in hex
//! (`0xc0000005` is an access violation, `101` is a Rust panic), and the tail of its log.
//!
//! `logging::pick_sink` writes to stderr whenever the process has one, so piping it is all it
//! takes to capture the app's own log on every platform. `RUST_BACKTRACE=1` goes on the child so
//! a panic names its frames.

use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::fixtures::create_fixture_image;

/// How much of the app's log a failure quotes. Enough for the whole of a short launch, and the
/// last thing it managed to say before dying in a long one.
const LOG_TAIL_LINES: usize = 60;

/// The child's stderr, drained by a thread of its own so a full pipe can never wedge the app.
#[derive(Clone, Default)]
pub struct AppLog(Arc<Mutex<Vec<String>>>);

impl AppLog {
    /// Start draining `stderr` into this log. The thread ends when the pipe closes, which is
    /// when the app exits.
    fn drain(&self, stderr: std::process::ChildStderr) {
        let lines = Arc::clone(&self.0);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                // Echoed as well as kept, so the app's log still lands in the test's own
                // captured output (and in a `--no-capture` run) the way it did when the child
                // simply inherited stderr.
                eprintln!("[prvw] {line}");
                if let Ok(mut lines) = lines.lock() {
                    lines.push(line);
                }
            }
        });
    }

    /// The last [`LOG_TAIL_LINES`] lines, indented so they read as a quotation inside a panic.
    fn tail(&self) -> String {
        let Ok(lines) = self.0.lock() else {
            return "    <the app log is poisoned>".to_string();
        };
        if lines.is_empty() {
            return "    <the app logged nothing>".to_string();
        }
        let start = lines.len().saturating_sub(LOG_TAIL_LINES);
        lines[start..]
            .iter()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub struct TestApp {
    // Behind a mutex so `&self` methods can ask whether the app is still alive: that answer is
    // the difference between "it crashed" and "it's wedged", and it's the first thing anyone
    // debugging a failed request wants.
    child: Mutex<Child>,
    log: AppLog,
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

    /// Start the app on several image files, in the order given. That order is what the app
    /// opens; the list it navigates is the same files in the user's sort order, so the two
    /// disagree on purpose whenever the first argument isn't the one that sorts first.
    pub fn start_with_images(images: &[&std::path::Path]) -> Self {
        Self::start_with_args_and_home(images, None)
    }

    /// Start the app with a single CLI argument (a file or directory) and an optional home
    /// override. The home override scopes the browse tree's home root so reveal walks are short
    /// and deterministic (the target sits directly under home).
    pub fn start_with_arg_and_home(arg: &std::path::Path, home: Option<&std::path::Path>) -> Self {
        Self::start_with_args_and_home(&[arg], home)
    }

    /// Start the app with the given CLI arguments and an optional home override.
    pub fn start_with_args_and_home(
        args: &[&std::path::Path],
        home: Option<&std::path::Path>,
    ) -> Self {
        // Find a free port by binding to :0, then closing the listener
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };

        // Fresh per-test settings dir — no cross-test leakage.
        let data_dir = tempfile::tempdir().expect("Couldn't create temp data dir");

        let mut command = Command::new(prvw_binary());
        command
            .args(args)
            .env("PRVW_QA_PORT", port.to_string())
            .env("PRVW_DATA_DIR", data_dir.path())
            // Open the window unfocused and behind everything so a run's swarm of test
            // windows doesn't grab the developer's keystrokes. Tests drive the app via
            // the QA HTTP server, not OS input, so this changes nothing they observe.
            .env("PRVW_BACKGROUND_WINDOW", "1")
            // The app's own log goes to stderr wherever the process has one (`logging::init`),
            // so piping it is what makes a Windows crash readable from a Mac. Draining it on a
            // thread of its own is not optional: a full pipe blocks the app's next log line.
            .stderr(Stdio::piped())
            // A panic that reaches the top of a thread prints its frames, which is the one line
            // that turns "the process died" into "the process died here".
            .env("RUST_BACKTRACE", "1");
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
        let mut child = command.spawn().expect("Failed to start prvw");
        let log = AppLog::default();
        if let Some(stderr) = child.stderr.take() {
            log.drain(stderr);
        }

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
                panic!(
                    "The QA server never came up on {base_url} within 10 seconds.\n{}\napp log:\n{}",
                    process_status(&mut child),
                    log.tail()
                );
            }
            if client.get(format!("{base_url}/state")).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let app = Self {
            child: Mutex::new(child),
            log,
            base_url,
            client,
            data_dir,
            _fixture_home: None,
        };
        app.wait_until_launched();
        // Let the live-sync watcher's control queue drain (it services watch requests on a
        // ~250 ms tick). A test that mutates the folder right away would otherwise race the watch.
        std::thread::sleep(Duration::from_millis(500));
        app
    }

    /// Block until the launch has settled: the opened image's folder has been scanned (so
    /// `total_files` is the real count rather than the provisional 1) and its pixels are up.
    ///
    /// Launch is asynchronous — the window paints before either finishes — so a fixed sleep would
    /// race it. A directory launch has no image and no image-mode scan of its own; the browse
    /// tests gate on the browse state instead.
    fn wait_until_launched(&self) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            let state = self.get_state();
            let scanned = state["scan_pending"].as_bool() != Some(true);
            let displayed = state["image_width"].as_u64().unwrap_or(0) > 0;
            if scanned && (displayed || state["view_mode"].as_str() == Some("browse")) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "The app didn't finish launching within 10 seconds.\n{}\napp log:\n{}",
            match self.child.lock() {
                Ok(mut child) => process_status(&mut child),
                Err(_) => "the child handle is poisoned".to_string(),
            },
            self.log.tail()
        );
    }

    /// The per-test `settings.json`. `PRVW_DATA_DIR` makes `settings::persistence::data_dir()`
    /// return the dir as-is (no platform-specific suffix), so the file sits directly inside.
    pub fn settings_path(&self) -> std::path::PathBuf {
        self.data_dir.path().join("settings.json")
    }

    /// Why a request to the app failed, in the words of whoever has to read it in a CI log.
    ///
    /// The transport error alone doesn't say much: a reset connection and a timeout look
    /// similar in a panic and mean opposite things. So this separates them, asks the OS whether
    /// the process is still there, and quotes the app's own log.
    fn diagnose(&self, request: &str, error: &reqwest::Error) -> String {
        let what = if error.is_timeout() {
            "the request timed out, so the app is alive but not answering (its main thread is \
             blocked, or a native modal is running its own message loop)"
        } else if error.is_connect() {
            "the connection couldn't be made at all"
        } else {
            "the connection failed mid-request, which is what a process that went away looks like"
        };
        let status = match self.child.lock() {
            Ok(mut child) => process_status(&mut child),
            Err(_) => "the child handle is poisoned".to_string(),
        };
        format!(
            "{request} failed: {what}.\n{status}\nerror: {error:?}\napp log:\n{}",
            self.log.tail()
        )
    }

    pub fn get_screenshot(&self) -> image::DynamicImage {
        let bytes = self
            .client
            .get(format!("{}/screenshot", self.base_url))
            .send()
            .unwrap_or_else(|error| panic!("{}", self.diagnose("GET /screenshot", &error)))
            .bytes()
            .expect("Failed to read screenshot bytes");
        image::load_from_memory(&bytes).expect("Failed to decode screenshot PNG")
    }

    pub fn get_state(&self) -> serde_json::Value {
        self.client
            .get(format!("{}/state", self.base_url))
            .send()
            .unwrap_or_else(|error| panic!("{}", self.diagnose("GET /state", &error)))
            .json()
            .expect("Failed to parse state JSON")
    }

    /// The parity table the app renders from the layer-1 registries (`src/parity/`). Same
    /// answer on every host, because the registries carry no `#[cfg]`.
    pub fn get_parity(&self) -> serde_json::Value {
        self.client
            .get(format!("{}/parity", self.base_url))
            .send()
            .unwrap_or_else(|error| panic!("{}", self.diagnose("GET /parity", &error)))
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
            let armed = state["watched_folders"].as_array().is_some_and(|f| {
                f.iter()
                    .filter_map(serde_json::Value::as_str)
                    .any(|p| names_one_folder(p, &wanted))
            });
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
            .unwrap_or_else(|error| panic!("{}", self.diagnose(&format!("POST {path}"), &error)))
            .json()
            .expect("Failed to parse response JSON")
    }

    pub fn post_json(&self, path: &str, json: &serde_json::Value) -> serde_json::Value {
        self.client
            .post(format!("{}{path}", self.base_url))
            .json(json)
            .send()
            .unwrap_or_else(|error| panic!("{}", self.diagnose(&format!("POST {path}"), &error)))
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
            .unwrap_or_else(|error| panic!("{}", self.diagnose("POST /mcp", &error)))
            .text()
            .expect("MCP response read failed");
        serde_json::from_str(&body).expect("MCP response is JSON")
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Whether the app is still running, and how it ended if it isn't.
///
/// The exit status is spelled in hex as well as decimal because that is the only form a Windows
/// crash code is legible in: `0xc0000005` is an access violation, `0xc0000409` is the abort a
/// panic unwinding out of a window procedure turns into, and plain `101` is an ordinary Rust
/// panic on the main thread.
fn process_status(child: &mut Child) -> String {
    match child.try_wait() {
        Ok(None) => "the app process is still running".to_string(),
        Ok(Some(status)) => match status.code() {
            #[allow(clippy::cast_sign_loss)]
            Some(code) => format!("the app process exited with {code} (0x{:08x})", code as u32),
            None => format!("the app process was terminated by a signal: {status}"),
        },
        Err(error) => format!("couldn't read the app process's status: {error}"),
    }
}

/// The `prvw` binary these tests drive, in order of preference.
///
/// `CARGO_BIN_EXE_prvw` is an absolute path baked in when the test binary is compiled, which is
/// right for `cargo test` on the machine that built it and wrong the moment the binary runs
/// anywhere else. Cross-compiling the suite for Windows from a Mac is exactly that case: the
/// baked path names a directory that doesn't exist on the machine running the tests. So:
///
/// 1. `PRVW_TEST_BINARY`, for pointing the suite at a specific build.
/// 2. The sibling of the test executable's own directory. Cargo puts test binaries in
///    `<target>/deps/` and the app one level up, so a copied target directory just works.
/// 3. The compile-time path, which is the normal local `cargo test` case.
fn prvw_binary() -> std::path::PathBuf {
    if let Some(explicit) = std::env::var_os("PRVW_TEST_BINARY") {
        return std::path::PathBuf::from(explicit);
    }
    let exe_name = if cfg!(windows) { "prvw.exe" } else { "prvw" };
    if let Some(sibling) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent()?.parent().map(|dir| dir.join(exe_name)))
        && sibling.is_file()
    {
        return sibling;
    }
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_prvw"))
}

/// Do these two paths name one folder? The harness's own copy of `paths::same_path`, because the
/// app is a binary and a test crate can't reach into it.
///
/// A test canonicalizes the folder it created, which on Windows returns `\\?\C:\…`, while the app
/// watches the folder its browse tree handed it, spelled the way a drive enumeration and
/// `read_dir` produced it (`C:\…`). NTFS calls those one folder and so does Prvw; only a byte
/// comparison disagrees. Off Windows there are no verbatim prefixes and case is significant, so
/// this is the byte comparison it always was.
fn names_one_folder(left: &str, right: &str) -> bool {
    if !cfg!(windows) {
        return left == right;
    }
    fn body(path: &str) -> String {
        let plain = match path.strip_prefix(r"\\?\UNC\") {
            // A share keeps the two separators that name it: `\\?\UNC\naspi\a` is `\\naspi\a`.
            Some(share) => format!(r"\\{share}"),
            None => path.strip_prefix(r"\\?\").unwrap_or(path).to_string(),
        };
        plain
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    }
    body(left) == body(right)
}
