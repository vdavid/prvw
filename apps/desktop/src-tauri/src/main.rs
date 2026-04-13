mod directory;
mod fe_log;
mod image_loader;
#[cfg(target_os = "macos")]
mod macos_open_handler;
mod mcp;
mod menu;
#[cfg(target_os = "macos")]
mod onboarding;
mod preloader;
#[allow(dead_code)] // Settings struct prepared for frontend wiring
mod settings;
#[cfg(target_os = "macos")]
mod updater;
#[allow(dead_code)] // Zoom/pan math retained for potential Rust-side use; currently handled in JS
mod view;

use clap::Parser;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

/// Snapshot of app state, updated by the main thread on every state change.
/// Shared between the MCP server (resources) and the main app.
#[derive(Clone, Debug)]
pub struct SharedAppState {
    pub current_file: Option<PathBuf>,
    pub current_index: usize,
    pub total_files: usize,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    pub fullscreen: bool,
    pub window_width: u32,
    pub window_height: u32,
    pub window_title: String,
    /// Pre-formatted diagnostics text, updated by the main thread.
    pub diagnostics_text: String,
}

impl Default for SharedAppState {
    fn default() -> Self {
        Self {
            current_file: None,
            current_index: 0,
            total_files: 0,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            fullscreen: false,
            window_width: 0,
            window_height: 0,
            window_title: String::new(),
            diagnostics_text: String::new(),
        }
    }
}

/// Commands sent from the Apple Event handler to the main app (via mpsc channel).
pub enum AppCommand {
    /// Open a specific file (from Finder double-click on a running instance).
    OpenFile(PathBuf),
}

#[derive(Parser)]
#[command(name = "prvw", about = "A fast, minimal image viewer")]
struct Cli {
    /// Path(s) to image file(s) to open
    files: Vec<PathBuf>,
}

/// App state managed by Tauri via `.manage()`. All access through `Mutex<AppState>`.
pub struct AppState {
    pub file_path: PathBuf,
    pub explicit_files: Option<Vec<PathBuf>>,
    pub dir_list: Option<directory::DirectoryList>,
    pub shared_state: Arc<Mutex<SharedAppState>>,
    pub onboarding_mode: bool,
    pub preloader: Option<preloader::Preloader>,
    pub image_cache: preloader::ImageCache,
    pub navigation_history: VecDeque<NavigationRecord>,
    pub current_image_size: Option<(u32, u32)>,
}

/// A record of a single navigation event, for performance diagnostics.
pub struct NavigationRecord {
    pub from_index: usize,
    pub to_index: usize,
    pub was_cached: bool,
    pub total_time: Duration,
    pub timestamp: Instant,
}

// ---------------------------------------------------------------------------
// Tauri command response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateResponse {
    file_path: Option<String>,
    index: usize,
    total: usize,
    onboarding: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NavigateResponse {
    file_path: Option<String>,
    index: usize,
    total: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OnboardingInfo {
    version: String,
    handler_status: String,
    not_in_applications: bool,
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_state(state: tauri::State<'_, Mutex<AppState>>) -> StateResponse {
    let state = state.lock().unwrap();
    let (file_path, index, total) = if let Some(dir) = &state.dir_list {
        (
            Some(dir.current().to_string_lossy().to_string()),
            dir.current_index(),
            dir.len(),
        )
    } else {
        (None, 0, 0)
    };
    StateResponse {
        file_path,
        index,
        total,
        onboarding: state.onboarding_mode,
    }
}

#[tauri::command]
fn navigate(
    forward: bool,
    state: tauri::State<'_, Mutex<AppState>>,
    window: tauri::WebviewWindow,
) -> NavigateResponse {
    let mut state = state.lock().unwrap();
    do_navigate(&mut state, forward, &window);
    response_from_state(&state)
}

#[tauri::command]
fn get_adjacent_paths(count: usize, state: tauri::State<'_, Mutex<AppState>>) -> Vec<String> {
    let state = state.lock().unwrap();
    let Some(dir) = &state.dir_list else {
        return Vec::new();
    };
    dir.preload_range(count)
        .iter()
        .filter_map(|&i| dir.get(i).map(|p| p.to_string_lossy().to_string()))
        .collect()
}

#[tauri::command]
fn toggle_fullscreen(
    state: tauri::State<'_, Mutex<AppState>>,
    window: tauri::WebviewWindow,
) -> bool {
    let is_fs = window.is_fullscreen().unwrap_or(false);
    set_fullscreen_impl(&state, &window, !is_fs)
}

#[tauri::command]
fn set_fullscreen(
    on: bool,
    state: tauri::State<'_, Mutex<AppState>>,
    window: tauri::WebviewWindow,
) -> bool {
    set_fullscreen_impl(&state, &window, on)
}

fn set_fullscreen_impl(
    state: &tauri::State<'_, Mutex<AppState>>,
    window: &tauri::WebviewWindow,
    target: bool,
) -> bool {
    let _ = window.set_fullscreen(target);

    let state = state.lock().unwrap();
    if let Ok(mut shared) = state.shared_state.lock() {
        shared.fullscreen = target;
    }
    target
}

#[tauri::command]
fn handle_escape(window: tauri::WebviewWindow) {
    if window.is_fullscreen().unwrap_or(false) {
        let _ = window.set_fullscreen(false);
    } else {
        let _ = window.close();
    }
}

#[tauri::command]
fn set_as_default_viewer() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        onboarding::set_as_default_viewer()?;
        Ok("Set Prvw as default viewer for all supported image types".to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Setting default viewer is only supported on macOS".to_string())
    }
}

#[tauri::command]
fn get_onboarding_info() -> OnboardingInfo {
    let version = env!("CARGO_PKG_VERSION").to_string();

    #[cfg(target_os = "macos")]
    let handler_status = onboarding::query_handler_status();
    #[cfg(not(target_os = "macos"))]
    let handler_status = String::new();

    #[cfg(target_os = "macos")]
    let not_in_applications = !onboarding::is_in_applications();
    #[cfg(not(target_os = "macos"))]
    let not_in_applications = false;

    OnboardingInfo {
        version,
        handler_status,
        not_in_applications,
    }
}

#[tauri::command]
fn report_zoom_pan(
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    window_width: u32,
    window_height: u32,
    state: tauri::State<'_, Mutex<AppState>>,
) {
    let state = state.lock().unwrap();
    if let Ok(mut shared) = state.shared_state.lock() {
        shared.zoom = zoom;
        shared.pan_x = pan_x;
        shared.pan_y = pan_y;
        shared.window_width = window_width;
        shared.window_height = window_height;
    }
}

#[tauri::command]
fn open_file(
    path: String,
    state: tauri::State<'_, Mutex<AppState>>,
    window: tauri::WebviewWindow,
) -> Result<NavigateResponse, String> {
    let resolved = PathBuf::from(&path)
        .canonicalize()
        .map_err(|e| format!("Couldn't resolve {path}: {e}"))?;

    if !resolved.is_file() {
        return Err(format!("Not a file: {}", resolved.display()));
    }

    let mut state = state.lock().unwrap();
    state.file_path = resolved.clone();
    state.dir_list = directory::DirectoryList::from_file(&resolved);
    state.onboarding_mode = false;

    // Update window title
    let filename = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Prvw");
    let _ = window.set_title(filename);

    update_shared_state(&state);
    Ok(response_from_state(&state))
}

/// Open (or focus) a dialog window, centered on the main window.
/// Uses logical coordinates and always-on-top for modal-like behavior.
pub(crate) fn do_open_dialog_window(
    app: &tauri::AppHandle,
    label: &str,
    title: &str,
    width: f64,
    height: f64,
) -> Result<(), String> {
    // If already open, just focus it
    if let Some(window) = app.get_webview_window(label) {
        window.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    // Center on the main window using logical coordinates
    let (x, y) = if let Some(main_window) = app.get_webview_window("main") {
        let scale = main_window.scale_factor().unwrap_or(1.0);
        let main_pos = main_window.outer_position().map_err(|e| e.to_string())?;
        let main_size = main_window.outer_size().map_err(|e| e.to_string())?;
        // outer_position/outer_size return physical pixels; convert to logical
        let lx = main_pos.x as f64 / scale;
        let ly = main_pos.y as f64 / scale;
        let lw = main_size.width as f64 / scale;
        let lh = main_size.height as f64 / scale;
        (lx + (lw - width) / 2.0, ly + (lh - height) / 2.0)
    } else {
        (200.0, 200.0)
    };

    let route = format!("/{label}");
    let window = tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::App(route.into()))
        .title(title)
        .inner_size(width, height)
        .resizable(false)
        .position(x, y)
        .always_on_top(true)
        .transparent(true)
        .build()
        .map_err(|e| e.to_string())?;

    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{NSVisualEffectMaterial, apply_vibrancy};
        let _ = apply_vibrancy(
            &window,
            NSVisualEffectMaterial::UnderWindowBackground,
            None,
            None,
        );
    }

    window.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn open_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    do_open_dialog_window(&app, "settings", "Settings", 480.0, 360.0)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
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

    let onboarding_mode = resolved_files.is_empty();

    if !onboarding_mode {
        if resolved_files.len() == 1 {
            log::info!("Opening {}", resolved_files[0].display());
        } else {
            log::info!("Opening {} files", resolved_files.len());
        }
    }

    let file_path = resolved_files.first().cloned().unwrap_or_default();
    let explicit_files = if resolved_files.len() > 1 {
        Some(resolved_files)
    } else {
        None
    };

    // Build directory list
    let dir_list = if onboarding_mode {
        None
    } else if let Some(files) = explicit_files.clone() {
        Some(directory::DirectoryList::from_explicit(files))
    } else {
        directory::DirectoryList::from_file(&file_path)
    };

    // Shared state for QA server
    let shared_state = Arc::new(Mutex::new(SharedAppState::default()));

    let app_state = AppState {
        file_path,
        explicit_files,
        dir_list,
        shared_state: Arc::clone(&shared_state),
        onboarding_mode,
        preloader: None,
        image_cache: preloader::ImageCache::new(),
        navigation_history: VecDeque::with_capacity(10),
        current_image_size: None,
    };

    // Update shared state with initial directory info
    update_shared_state(&app_state);

    // Command channel: Apple Event handler sends OpenFile commands through this
    #[cfg(target_os = "macos")]
    let (command_tx, command_rx) = std::sync::mpsc::channel::<AppCommand>();
    #[cfg(target_os = "macos")]
    let open_handler_tx = command_tx.clone();
    #[cfg(target_os = "macos")]
    drop(command_tx); // only the clone is used

    tauri::Builder::default()
        .plugin({
            use tauri_plugin_log::{Target, TargetKind};

            fn parse_level_filter(s: &str) -> Option<log::LevelFilter> {
                match s.to_lowercase().as_str() {
                    "trace" => Some(log::LevelFilter::Trace),
                    "debug" => Some(log::LevelFilter::Debug),
                    "info" => Some(log::LevelFilter::Info),
                    "warn" => Some(log::LevelFilter::Warn),
                    "error" => Some(log::LevelFilter::Error),
                    "off" => Some(log::LevelFilter::Off),
                    _ => None,
                }
            }

            let mut builder = tauri_plugin_log::Builder::new()
                .targets([Target::new(TargetKind::Stdout)])
                .format(|out, message, record| {
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
                    out.finish(format_args!(
                        "{ts} {color}{level:<5}\x1b[0m {target:<16} {message}"
                    ))
                })
                .level_for("tao", log::LevelFilter::Warn)
                .level_for("muda", log::LevelFilter::Warn);

            // Parse RUST_LOG env var for per-module level overrides
            if let Ok(rust_log) = std::env::var("RUST_LOG") {
                let mut base_level = log::LevelFilter::Info;
                for directive in rust_log.split(',') {
                    let directive = directive.trim();
                    if directive.is_empty() {
                        continue;
                    }
                    if let Some((module, level_str)) = directive.split_once('=') {
                        if let Some(level) = parse_level_filter(level_str) {
                            builder = builder.level_for(module.to_string(), level);
                        }
                    } else if let Some(level) = parse_level_filter(directive) {
                        base_level = level;
                    }
                }
                builder = builder.level(base_level);
            } else {
                builder = builder.level(log::LevelFilter::Info);
            }

            builder.build()
        })
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(Mutex::new(app_state))
        .invoke_handler(tauri::generate_handler![
            get_state,
            navigate,
            get_adjacent_paths,
            toggle_fullscreen,
            set_fullscreen,
            handle_escape,
            set_as_default_viewer,
            get_onboarding_info,
            report_zoom_pan,
            open_file,
            open_settings_window,
            fe_log::batch_fe_logs,
        ])
        .menu(menu::build_menu)
        .on_menu_event(menu::handle_menu_event)
        .setup(move |app| {
            let window = app
                .get_webview_window("main")
                .expect("main window not found");

            if onboarding_mode {
                let _ = window.set_size(tauri::LogicalSize::new(560.0, 400.0));
                let _ = window.set_title("Welcome to Prvw");

                #[cfg(target_os = "macos")]
                {
                    use window_vibrancy::{NSVisualEffectMaterial, apply_vibrancy};
                    let _ = apply_vibrancy(
                        &window,
                        NSVisualEffectMaterial::UnderWindowBackground,
                        None,
                        None,
                    );
                }
            } else {
                // Set window title to filename
                let state = app.state::<Mutex<AppState>>();
                let state = state.lock().unwrap();
                if let Some(dir) = &state.dir_list {
                    let filename = dir
                        .current()
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("Prvw");
                    let _ = window.set_title(filename);
                }
            }

            // Image preloading is handled by the frontend (browser image cache).
            // The Rust preloader module is retained for MCP screenshot support only.

            // Start MCP server
            let mcp_config = mcp::McpConfig::from_env();
            if mcp_config.enabled {
                let mcp_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = mcp::start_mcp_server(mcp_handle, mcp_config).await {
                        log::error!("MCP server failed to start: {e}");
                    }
                });
            }

            // Apple Event handler (macOS): files opened via Finder send through
            // the command channel, polled by a lightweight thread.
            #[cfg(target_os = "macos")]
            {
                let apple_event_handle = app.handle().clone();
                std::thread::Builder::new()
                    .name("prvw-apple-event-poller".into())
                    .spawn(move || {
                        while let Ok(cmd) = command_rx.recv() {
                            match cmd {
                                AppCommand::OpenFile(path) => {
                                    apple_event_handle
                                        .emit("qa-open-file", path.to_string_lossy().to_string())
                                        .ok();
                                }
                            }
                        }
                    })
                    .expect("Failed to spawn Apple Event poller thread");

                let _handler = macos_open_handler::register(open_handler_tx);
                // Leak the handler to keep it alive for the app's lifetime.
                std::mem::forget(_handler);
            }

            // Check for updates (respects the "check for updates" setting)
            #[cfg(target_os = "macos")]
            {
                let app_data_dir = app.path().app_data_dir().unwrap_or_default();
                let user_settings = settings::load_from_store(&app_data_dir);
                if user_settings.updates_enabled {
                    updater::check_and_update();
                } else {
                    log::info!("Update check disabled by user setting");
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error running Prvw");
}

// ---------------------------------------------------------------------------
// Navigation and state helpers
// ---------------------------------------------------------------------------

fn do_navigate(state: &mut AppState, forward: bool, window: &tauri::WebviewWindow) {
    let from_index = state
        .dir_list
        .as_ref()
        .map(|d| d.current_index())
        .unwrap_or(0);

    let moved = if let Some(dir) = &mut state.dir_list {
        if forward {
            dir.go_next()
        } else {
            dir.go_prev()
        }
    } else {
        false
    };

    if !moved {
        return;
    }

    let nav_start = Instant::now();

    let (current_path, current_index) = {
        let dir = state.dir_list.as_ref().unwrap();
        (dir.current().to_path_buf(), dir.current_index())
    };

    let was_cached = state.image_cache.contains(current_index);

    // Update file path
    state.file_path = current_path;

    // Record navigation timing
    let total_time = nav_start.elapsed();
    if state.navigation_history.len() >= 10 {
        state.navigation_history.pop_front();
    }
    state.navigation_history.push_back(NavigationRecord {
        from_index,
        to_index: current_index,
        was_cached,
        total_time,
        timestamp: Instant::now(),
    });

    // Update window title
    let filename = state
        .file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Prvw");
    let _ = window.set_title(filename);

    update_shared_state(state);
}

/// Kick off preloading for images adjacent to the current position.
/// Currently unused (browser handles preloading), retained for potential future use.
#[allow(dead_code)]
fn start_preloading(state: &mut AppState) {
    let Some(dir) = &state.dir_list else { return };
    let preload_indices = dir.preload_range(preloader::preload_count());

    let to_preload: Vec<(usize, PathBuf)> = preload_indices
        .iter()
        .filter(|&&i| !state.image_cache.contains(i))
        .filter_map(|&i| dir.get(i).map(|p| (i, p.to_path_buf())))
        .collect();

    if !to_preload.is_empty()
        && let Some(preloader) = &mut state.preloader
    {
        preloader.request_preload(to_preload);
    }
}

/// Process a single preload response (called by the receiver thread).
/// Currently unused (browser handles preloading), retained for potential future use.
#[allow(dead_code)]
fn handle_preload_response(state: &mut AppState, response: preloader::PreloadResponse) {
    match response {
        preloader::PreloadResponse::Ready {
            index,
            image,
            decode_duration,
            file_name,
        } => {
            if let Some(preloader) = &mut state.preloader {
                preloader.mark_complete(index);
            }
            state
                .image_cache
                .insert(index, image, decode_duration, file_name);
        }
        preloader::PreloadResponse::Failed {
            index,
            path,
            reason,
        } => {
            if let Some(preloader) = &mut state.preloader {
                preloader.mark_complete(index);
            }
            log::debug!(
                "Preload response: failed [{index}] {}: {reason}",
                path.display()
            );
        }
        preloader::PreloadResponse::Cancelled { index } => {
            if let Some(preloader) = &mut state.preloader {
                preloader.mark_complete(index);
            }
        }
    }
}

fn update_shared_state(state: &AppState) {
    let Ok(mut shared) = state.shared_state.lock() else {
        return;
    };

    if let Some(dir) = &state.dir_list {
        shared.current_file = Some(dir.current().to_path_buf());
        shared.current_index = dir.current_index();
        shared.total_files = dir.len();

        let title = dir
            .current()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Prvw");
        shared.window_title = title.to_string();
    }

    shared.diagnostics_text = build_diagnostics_text(state);
}

fn response_from_state(state: &AppState) -> NavigateResponse {
    let (file_path, index, total) = if let Some(dir) = &state.dir_list {
        (
            Some(dir.current().to_string_lossy().to_string()),
            dir.current_index(),
            dir.len(),
        )
    } else {
        (None, 0, 0)
    };
    NavigateResponse {
        file_path,
        index,
        total,
    }
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

fn build_diagnostics_text(state: &AppState) -> String {
    let current_index = state
        .dir_list
        .as_ref()
        .map(|d| d.current_index())
        .unwrap_or(0);

    let mut out = String::new();

    let cache_diag = state.image_cache.diagnostics();
    out.push_str("cache:\n");
    out.push_str(&format!(
        "  total_memory: {}\n",
        format_bytes(cache_diag.total_memory)
    ));
    out.push_str(&format!(
        "  entries: {} of {} budget\n",
        cache_diag.entries.len(),
        format_bytes(cache_diag.memory_budget)
    ));
    if !cache_diag.entries.is_empty() {
        out.push_str("  images:\n");
        for entry in &cache_diag.entries {
            let current_marker = if entry.index == current_index {
                "  \u{2190} current"
            } else {
                ""
            };
            out.push_str(&format!(
                "    [{}] {}  {}x{}  {}  decoded in {}ms{}\n",
                entry.index,
                entry.file_name,
                entry.width,
                entry.height,
                format_bytes(entry.memory_bytes),
                entry.decode_duration.as_millis(),
                current_marker,
            ));
        }
    }

    out.push_str("\npreloader:\n");
    out.push_str(&format!(
        "  window: current \u{00b1} {}\n",
        preloader::preload_count()
    ));

    out.push_str("\nrecent_navigations (newest first):\n");
    if state.navigation_history.is_empty() {
        out.push_str("  (none)\n");
    } else {
        let now = Instant::now();
        for record in state.navigation_history.iter().rev() {
            let ago = now.duration_since(record.timestamp);
            let cached_str = if record.was_cached { "yes" } else { "no " };
            out.push_str(&format!(
                "  {}\u{2192}{}  cached: {}  display: {}ms  {:.1}s ago\n",
                record.from_index,
                record.to_index,
                cached_str,
                record.total_time.as_millis(),
                ago.as_secs_f64(),
            ));
        }
    }

    let process_memory = get_process_rss_mb();
    out.push_str(&format!(
        "\nprocess_memory: {:.1} MB (cache: {})\n",
        process_memory,
        format_bytes(cache_diag.total_memory)
    ));

    out
}

fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn get_process_rss_mb() -> f64 {
    let pid = std::process::id();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            text.trim().parse::<f64>().ok()
        })
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}
