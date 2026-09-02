//! Prvw — the binary entry point.
//!
//! Parses the CLI, sets up logging, creates the winit event loop, and hands off to
//! `app::App` which owns all runtime state.

// No console window behind the app on Windows. It costs us stderr, which `logging` gets back
// through the parent console or a log file. Every other target ignores this attribute.
#![windows_subsystem = "windows"]
// `serde_json::json!` recurses once per entry, and the QA server's `/state` object has more
// entries than the default 128 frames allow. Every field of it is observable app state a test
// asserts on, so the object grows with the app.
#![recursion_limit = "256"]

// Infrastructure
mod app;
// The colour policy behind every Win32 window Prvw puts up: which theme, which surface, which
// ink. Pure, so a Mac can assert what a Windows user will see; `platform::windows::dark_mode` is
// the half that calls Win32. macOS and Linux never paint a Win32 window, so on those two the
// module is only its own tests' subject.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod chrome;
// Windows' clipboard formats are byte layouts, so they're written and tested here rather than
// behind a `#[cfg]` (`clipboard.rs` says why). macOS builds `NSPasteboard` objects instead and
// Linux has no clipboard yet, so on those two the module is only its own tests' subject.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod clipboard;
mod commands;
mod folder_scan;
mod folder_watch;
mod input;
mod launch;
mod logging;
mod menu;
// The registries answer for every platform at once, so whatever a given build's own UI doesn't
// consume is unused there: Linux has no menu bar and no settings window, and Windows has a menu
// bar but no settings window until M4. macOS has all of it, so it's the build that still catches
// a registry entry nothing reads any more.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod parity;
mod paths;
mod pixels;
mod platform;
// Page layout and the pixel form a GDI printer DC wants, both portable so any host can test
// them. macOS and Windows print; Linux has no print path, so there the module is only its own
// tests' subject.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
mod printing;
mod render;
mod scroll;

// Features
// What the About box says lives in `about::content`, which compiles everywhere: a Mac's test run
// checks all three platforms' copy and the `SysLink` markup Windows renders it with. Only the
// Windows build consumes all of it. macOS lays the same strings out in AppKit and shows no licence
// line, and Linux has no About window to open at all (`menu/absent.rs`), so on those two the
// module is partly its own tests' subject.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod about;
mod browser;
mod color;
mod decoding;
mod diagnostics;
mod exif_overlay;
#[cfg(target_os = "macos")]
mod file_associations;
mod histogram;
mod navigation;
#[cfg(target_os = "macos")]
mod onboarding;
mod open_dialog;
// Two halves with different reach. `metadata` and `dim_prefetch` size the window before the first
// pixel paints and run on every platform; the scheduler and the preview cache need a generator to
// feed them, which macOS and Windows have and Linux doesn't, so there they have no consumer at
// all. The two platforms with one still catch a member nothing reads any more.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
mod previews;
mod qa;
mod settings;
mod slideshow;
// Two halves again. `updater::manifest` decides whether a newer release exists and is pure, so
// every host compiles and tests it; the acting half only exists where there's a way to deliver
// an update, which leaves Linux with the policy and no caller for it.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
mod updater;
mod window;
mod zoom;

use app::App;
use app::SharedAppState;
use clap::Parser;
use commands::AppCommand;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use winit::event_loop::{ControlFlow, EventLoop};

/// Height of the title bar area in logical pixels. When the title bar setting is on,
/// the image area starts this many pixels below the top of the window.
pub(crate) const TITLE_BAR_HEIGHT: f32 = 32.0;

#[derive(Parser)]
#[command(name = "prvw", about = "A fast, minimal image viewer")]
struct Cli {
    /// Path(s) to open: image file(s) for image mode, or a single directory to browse
    files: Vec<PathBuf>,
}

fn main() {
    logging::init();

    let version = env!("CARGO_PKG_VERSION");
    log::info!("Prvw {version} starting");

    let cli = Cli::parse();

    // A single directory argument launches browse mode at that folder. Detect it before the
    // file-filtering pass (which would otherwise drop it as "not a file"). Classification is the
    // pure `browser::classify_launch_target`; we read `is_file`/`is_dir` once off the canonical
    // path here. Multiple args are always treated as an image set (no "browse two folders").
    let mut launch_directory: Option<PathBuf> = None;
    if cli.files.len() == 1
        && let Ok(canonical) = cli.files[0].canonicalize()
        && matches!(
            browser::classify_launch_target(canonical.is_file(), canonical.is_dir()),
            browser::LaunchTarget::Directory
        )
    {
        log::info!("Launching browse mode at directory {}", canonical.display());
        launch_directory = Some(canonical);
    }

    let resolved_files: Vec<PathBuf> = if launch_directory.is_some() {
        Vec::new()
    } else {
        cli.files
            .iter()
            .filter_map(|f| match f.canonicalize() {
                Ok(p) if p.is_file() => Some(p),
                Ok(p) => {
                    log::warn!("Not a file, skipping: {}", p.display());
                    None
                }
                Err(e) => {
                    log::warn!("Couldn't resolve {}: {e}", f.display());
                    None
                }
            })
            .collect()
    };

    // Onboarding (the "waiting" path) only where there's something to wait for: macOS, where
    // Finder is about to deliver the file through an Apple Event. Everywhere else the window
    // comes up on its empty state and File → Open is the way in. See `launch::waits_for_a_file`.
    let nothing_named = resolved_files.is_empty() && launch_directory.is_none();
    let waiting_for_file = launch::waits_for_a_file(nothing_named, parity::Platform::HOST);

    if launch_directory.is_some() {
        // Already logged above.
    } else if waiting_for_file {
        log::info!("No files on CLI, waiting for Apple Event (Finder double-click)");
    } else if nothing_named {
        log::info!("Nothing to show yet. Opening an empty window.");
    } else if resolved_files.len() == 1 {
        log::info!("Opening {}", resolved_files[0].display());
    } else {
        log::info!("Opening {} files", resolved_files.len());
    }

    let file_path = resolved_files.first().cloned().unwrap_or_default();

    let mut event_loop_builder = EventLoop::<AppCommand>::with_user_event();
    // Test mode: never let the app become the active application, so a run's swarm of
    // windows can't steal the developer's keystrokes.
    //   - activation policy `Prohibited`: the app *cannot* be activated at all. This is
    //     the robust lever — it defeats every focus path (winit's launch activation, the
    //     window-activation hack, a settings window's `makeKeyAndOrderFront`) at once,
    //     because none of them can activate a Prohibited app.
    //   - `activate_ignoring_other_apps(false)`: also stop winit's launch-time
    //     `activateIgnoringOtherApps(true)`, belt and suspenders.
    // Paired with `with_active(false)` + `orderBack:` in `window.rs` (and `order_window_in`
    // for the AppKit settings/about windows) so the windows also stay visually behind.
    // See `window::background_window_requested`.
    #[cfg(target_os = "macos")]
    if window::background_window_requested() {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        event_loop_builder
            .with_activation_policy(ActivationPolicy::Prohibited)
            .with_activate_ignoring_other_apps(false);
    }
    // The one message hook winit allows, shared by the menu's accelerators and (from M4)
    // modeless dialogs' keyboard navigation. Must go on before `build()`.
    #[cfg(target_os = "windows")]
    platform::windows::msg_hook::install(&mut event_loop_builder);

    let event_loop = event_loop_builder
        .build()
        .expect("Failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let shared_state = Arc::new(Mutex::new(SharedAppState::default()));

    // Inject application:openURLs: into winit's delegate class so macOS routes file-open
    // events to us instead of NSDocumentController (which shows "cannot open files" errors).
    // Must happen after EventLoop::new() (which creates the WinitApplicationDelegate class)
    // but before run_app() (which calls finishLaunching and dispatches queued Apple Events).
    #[cfg(target_os = "macos")]
    {
        use platform::macos::open_handler;
        open_handler::set_proxy(proxy.clone());
        open_handler::register();
    }

    let explicit_files = if resolved_files.len() > 1 {
        Some(resolved_files)
    } else {
        None
    };

    let mut app = App::new(
        file_path,
        explicit_files,
        waiting_for_file,
        launch_directory,
        proxy,
        Arc::clone(&shared_state),
    );
    event_loop
        .run_app(&mut app)
        .expect("Event loop terminated unexpectedly");
}
