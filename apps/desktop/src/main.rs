//! Prvw — the binary entry point.
//!
//! Parses the CLI, sets up logging, creates the winit event loop, and hands off to
//! `app::App` which owns all runtime state.

// Infrastructure
mod app;
mod commands;
mod input;
mod menu;
mod pixels;
mod platform;
mod render;

// Features
#[cfg(target_os = "macos")]
mod about;
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
mod qa;
mod settings;
#[cfg(target_os = "macos")]
mod thumbnails;
#[cfg(target_os = "macos")]
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
    /// Path(s) to image file(s) to open
    files: Vec<PathBuf>,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .filter_module("wgpu", log::LevelFilter::Warn)
        .filter_module("wgpu_core", log::LevelFilter::Warn)
        .filter_module("wgpu_hal", log::LevelFilter::Warn)
        .filter_module("naga", log::LevelFilter::Warn)
        .filter_module("muda", log::LevelFilter::Warn)
        .format(|buf, record| {
            use std::io::Write;
            let now = chrono::Local::now();
            let ts = now.format("%H:%M:%S%.3f");
            let target = record
                .target()
                .strip_prefix("prvw::")
                .unwrap_or(record.target());
            let level = record.level();
            let color = match level {
                log::Level::Error => "\x1b[31m",
                log::Level::Warn => "\x1b[33m",
                log::Level::Info => "\x1b[32m",
                log::Level::Debug => "\x1b[36m",
                log::Level::Trace => "\x1b[35m",
            };
            writeln!(
                buf,
                "{ts} {color}{level:<5}\x1b[0m {target:<16} {}",
                record.args()
            )
        })
        .init();

    let version = env!("CARGO_PKG_VERSION");
    log::info!("Prvw {version} starting");

    let cli = Cli::parse();

    let resolved_files: Vec<PathBuf> = cli
        .files
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
        .collect();

    let waiting_for_file = resolved_files.is_empty();

    if waiting_for_file {
        log::info!("No files on CLI, waiting for Apple Event (Finder double-click)");
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
        proxy,
        Arc::clone(&shared_state),
    );
    event_loop
        .run_app(&mut app)
        .expect("Event loop terminated unexpectedly");
}
