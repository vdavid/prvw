mod directory;
mod image_loader;
mod input;
#[cfg(target_os = "macos")]
mod macos_open_handler;
mod menu;
#[cfg(target_os = "macos")]
mod native_ui;
#[cfg(target_os = "macos")]
mod onboarding;
mod preloader;
mod qa_server;
mod renderer;
mod settings;
mod text;
#[cfg(target_os = "macos")]
mod updater;
mod view;
mod window;

use clap::Parser;
use qa_server::{AppCommand, SharedAppState};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

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

    // No files passed: show the native onboarding window and exit.
    // This runs BEFORE EventLoop::new() to avoid nested run loop segfaults
    // (see AGENTS.md gotcha on AppKit modals).
    if resolved_files.is_empty() {
        log::info!("No files passed, showing onboarding window");
        #[cfg(target_os = "macos")]
        native_ui::show_onboarding_window();
        return;
    }

    if resolved_files.len() == 1 {
        log::info!("Opening {}", resolved_files[0].display());
    } else {
        log::info!("Opening {} files", resolved_files.len());
    }

    let file_path = resolved_files.first().cloned().unwrap_or_default();

    let event_loop = EventLoop::<AppCommand>::with_user_event()
        .build()
        .expect("Failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let shared_state = Arc::new(Mutex::new(SharedAppState::default()));

    // Register Apple Event handler for file-open events (double-click while already running).
    // Uses NSAppleEventManager, not NSApplicationDelegate (which would conflict with winit).
    #[cfg(target_os = "macos")]
    let _open_handler = macos_open_handler::register(proxy.clone());

    let explicit_files = if resolved_files.len() > 1 {
        Some(resolved_files)
    } else {
        None
    };

    let mut app = App::new(file_path, explicit_files, proxy, Arc::clone(&shared_state));
    event_loop
        .run_app(&mut app)
        .expect("Event loop terminated unexpectedly");
}

/// A record of a single navigation event, for performance diagnostics.
pub struct NavigationRecord {
    pub from_index: usize,
    pub to_index: usize,
    pub was_cached: bool,
    pub total_time: Duration,
    pub timestamp: Instant,
}

/// Application state, created before the event loop starts.
/// The window and renderer are initialized in `resumed()` (required by winit 0.30 on macOS).
struct App {
    file_path: PathBuf,
    /// If multiple files were passed on the CLI, use them as the navigation set instead of
    /// scanning the directory.
    explicit_files: Option<Vec<PathBuf>>,
    window: Option<Arc<Window>>,
    renderer: Option<renderer::Renderer>,
    view_state: view::ViewState,
    app_menu: Option<menu::AppMenu>,
    dir_list: Option<directory::DirectoryList>,
    preloader: Option<preloader::Preloader>,
    image_cache: preloader::ImageCache,
    /// Keyboard modifier state (Cmd, Shift, etc.)
    modifiers: ModifiersState,
    /// Mouse drag tracking
    drag_start: Option<(f64, f64)>,
    last_mouse_pos: (f64, f64),
    /// Double-click detection
    last_click_time: Option<Instant>,
    /// Whether we need to re-render next frame
    needs_redraw: bool,
    /// QA server shared state and event loop proxy
    shared_state: Arc<Mutex<SharedAppState>>,
    event_loop_proxy: EventLoopProxy<AppCommand>,
    /// Handle to the QA server thread (kept alive for the app's lifetime)
    _qa_handle: Option<std::thread::JoinHandle<()>>,
    /// Recent navigation records for performance diagnostics (newest last, cap 10).
    navigation_history: VecDeque<NavigationRecord>,
    /// Current image dimensions (stored so resize can update the view without needing the cache).
    current_image_size: Option<(u32, u32)>,
    /// Whether the window auto-resizes to fit each loaded image.
    auto_fit_window: bool,
    /// Whether small images are enlarged to fill the window.
    enlarge_small_images: bool,
}

impl App {
    fn new(
        file_path: PathBuf,
        explicit_files: Option<Vec<PathBuf>>,
        event_loop_proxy: EventLoopProxy<AppCommand>,
        shared_state: Arc<Mutex<SharedAppState>>,
    ) -> Self {
        let initial_settings = settings::Settings::load();
        Self {
            file_path,
            explicit_files,
            window: None,
            renderer: None,
            view_state: view::ViewState::new(),
            app_menu: None,
            dir_list: None,
            preloader: None,
            image_cache: preloader::ImageCache::new(),
            modifiers: ModifiersState::empty(),
            drag_start: None,
            last_mouse_pos: (0.0, 0.0),
            last_click_time: None,
            needs_redraw: false,
            shared_state,
            event_loop_proxy,
            _qa_handle: None,
            navigation_history: VecDeque::with_capacity(10),
            current_image_size: None,
            auto_fit_window: initial_settings.auto_fit_window,
            enlarge_small_images: initial_settings.enlarge_small_images,
        }
    }

    /// Recalculate the zoom floor based on current image/window/settings state.
    /// Called on image load, window resize, and setting changes. Does NOT change the
    /// current zoom level (only reclamps if it's below the new floor).
    fn update_min_zoom(&mut self) {
        let actual_zoom = self.view_state.actual_size_zoom();
        let allow_sub_fit =
            actual_zoom < 1.0 && !self.enlarge_small_images && !self.auto_fit_window;
        self.view_state
            .set_min_zoom(if allow_sub_fit { actual_zoom } else { 1.0 });
    }

    /// Choose the right initial zoom for a newly loaded image.
    /// Sets both the zoom floor and the starting zoom level.
    fn apply_initial_zoom(&mut self) {
        self.update_min_zoom();
        let actual_zoom = self.view_state.actual_size_zoom();
        let is_small = actual_zoom < 1.0;

        if is_small && !self.enlarge_small_images && !self.auto_fit_window {
            self.view_state.actual_size();
        } else {
            self.view_state.fit_to_window();
        }
    }

    /// Load and display an image, updating the renderer and view state.
    fn display_image(&mut self, path: &Path) {
        if self.renderer.is_none() {
            return;
        }

        let filename = path.file_name().unwrap_or_default().to_string_lossy();

        match image_loader::load_image(path) {
            Ok(image) => {
                self.current_image_size = Some((image.width, image.height));

                let renderer = self.renderer.as_mut().unwrap();

                // Resize window to match image (if enabled and not fullscreen).
                // Use the returned physical size directly — request_inner_size is async,
                // so window.inner_size() would still return the OLD size.
                if self.auto_fit_window
                    && let Some(win) = &self.window
                    && let Some(size) = window::resize_to_fit_image(win, image.width, image.height)
                {
                    renderer.resize(size.width, size.height);
                }

                self.view_state.update_dimensions(
                    image.width,
                    image.height,
                    renderer.surface_width(),
                    renderer.surface_height(),
                );
                renderer.set_image(&image);
                // Drop the renderer borrow before apply_initial_zoom (which borrows &mut self)
                self.apply_initial_zoom();
                self.renderer
                    .as_ref()
                    .unwrap()
                    .update_transform(&self.view_state.transform());
                self.request_redraw();

                if let Some(dir) = &self.dir_list {
                    log::info!(
                        "Displayed {filename} ({}/{})",
                        dir.current_index() + 1,
                        dir.len()
                    );
                } else {
                    log::info!("Displayed {filename}");
                }
            }
            Err(msg) => {
                log::error!("{msg}");
                if let Some(win) = &self.window {
                    win.set_title(&format!("Prvw - {msg}"));
                }
            }
        }
    }

    /// Display an image from the cache or load it fresh.
    fn display_cached_or_load(
        &mut self,
        index: usize,
        path: PathBuf,
        current_index: usize,
        total: usize,
    ) {
        if self.renderer.is_none() {
            return;
        }

        if let Some(image) = self.image_cache.get(index) {
            self.current_image_size = Some((image.width, image.height));

            let renderer = self.renderer.as_mut().unwrap();

            if self.auto_fit_window
                && let Some(win) = &self.window
                && let Some(size) = window::resize_to_fit_image(win, image.width, image.height)
            {
                renderer.resize(size.width, size.height);
            }

            self.view_state.update_dimensions(
                image.width,
                image.height,
                renderer.surface_width(),
                renderer.surface_height(),
            );
            renderer.set_image(image);
            self.apply_initial_zoom();
            self.renderer
                .as_ref()
                .unwrap()
                .update_transform(&self.view_state.transform());
            self.request_redraw();
        } else {
            if let Some(win) = &self.window {
                win.set_title(&window::window_title_loading(current_index, total));
            }
            self.display_image(&path);
        }
    }

    fn navigate(&mut self, forward: bool) {
        let from_index = self
            .dir_list
            .as_ref()
            .map(|d| d.current_index())
            .unwrap_or(0);

        let moved = if let Some(dir) = &mut self.dir_list {
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
        let direction = if forward { "next" } else { "prev" };

        // Extract what we need from dir_list before mutable borrow
        let (current_path, current_index, total, preload_indices) = {
            let dir = self.dir_list.as_ref().unwrap();
            let indices = dir.preload_range(preloader::preload_count());
            (
                dir.current().to_path_buf(),
                dir.current_index(),
                dir.len(),
                indices,
            )
        };

        let was_cached = self.image_cache.contains(current_index);
        let cached_str = if was_cached { "yes" } else { "no" };
        log::debug!("Navigate {direction}: {from_index} -> {current_index} (cached: {cached_str})");

        // Update window title
        if let Some(win) = &self.window {
            win.set_title(&window::window_title_with_position(
                &current_path,
                current_index,
                total,
            ));
        }

        // Display the current image
        self.display_cached_or_load(current_index, current_path, current_index, total);

        // Record navigation timing
        let total_time = nav_start.elapsed();
        if self.navigation_history.len() >= 10 {
            self.navigation_history.pop_front();
        }
        self.navigation_history.push_back(NavigationRecord {
            from_index,
            to_index: current_index,
            was_cached,
            total_time,
            timestamp: Instant::now(),
        });

        // Cancel stale preload tasks and submit fresh ones for adjacent images
        if let Some(dir) = &self.dir_list {
            let to_preload: Vec<(usize, PathBuf)> = preload_indices
                .iter()
                .filter(|&&i| !self.image_cache.contains(i))
                .filter_map(|&i| dir.get(i).map(|p| (i, p.to_path_buf())))
                .collect();

            if let Some(preloader) = &mut self.preloader {
                preloader.request_preload(to_preload);
            }
        }

        self.update_shared_state();
    }

    fn update_transform_and_redraw(&mut self) {
        log::debug!(
            "View: zoom={:.2}, pan=({:.2}, {:.2})",
            self.view_state.zoom,
            self.view_state.pan_x,
            self.view_state.pan_y
        );
        if let Some(renderer) = &self.renderer {
            renderer.update_transform(&self.view_state.transform());
        }
        self.request_redraw();
        self.update_shared_state();
    }

    fn request_redraw(&mut self) {
        self.needs_redraw = true;
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    /// Build text blocks for the header overlay in the transparent titlebar area.
    /// Shows filename, position, and zoom level (for example, "3 / 60 - photo.jpg  |  150%").
    fn build_text_overlay(&self) -> Vec<text::TextBlock> {
        let Some(renderer) = &self.renderer else {
            return Vec::new();
        };
        self.build_header_text(renderer.surface_width())
    }

    /// Build header text blocks shown in the transparent titlebar area during image viewing.
    fn build_header_text(&self, screen_width: u32) -> Vec<text::TextBlock> {
        let Some(dir) = &self.dir_list else {
            return Vec::new();
        };

        let filename = dir
            .current()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Prvw");

        let position = if dir.len() > 1 {
            format!("{} / {} \u{2013} ", dir.current_index() + 1, dir.len())
        } else {
            String::new()
        };

        let zoom_pct = (self.view_state.zoom * 100.0).round() as i32;
        let header = format!("{position}{filename}  |  {zoom_pct}%");

        // Semi-transparent white, positioned in the titlebar area (about 12px from top,
        // centered horizontally). On macOS with transparent titlebar, the standard title
        // bar is ~28px tall. We render our text roughly centered in that area.
        let header_color: [u8; 4] = [220, 220, 225, 200];

        // Approximate center: estimate text width as ~7px per char at font size 13
        let approx_text_width = header.len() as f32 * 7.0;
        let x = ((screen_width as f32 - approx_text_width) / 2.0).max(80.0);

        vec![text::TextBlock {
            text: header,
            x,
            y: 6.0,
            font_size: 13.0,
            line_height: 18.0,
            color: header_color,
            max_width: Some(screen_width as f32 - 160.0),
        }]
    }

    /// Drain preloader responses and cache the results.
    fn poll_preloader(&mut self) {
        let Some(preloader) = &mut self.preloader else {
            return;
        };
        while let Ok(response) = preloader.response_rx.try_recv() {
            match response {
                preloader::PreloadResponse::Ready {
                    index,
                    image,
                    decode_duration,
                    file_name,
                } => {
                    preloader.mark_complete(index);
                    self.image_cache
                        .insert(index, image, decode_duration, file_name);
                }
                preloader::PreloadResponse::Failed {
                    index,
                    path,
                    reason,
                } => {
                    preloader.mark_complete(index);
                    log::debug!(
                        "Preload response: failed [{index}] {}: {reason}",
                        path.display()
                    );
                }
                preloader::PreloadResponse::Cancelled { index } => {
                    preloader.mark_complete(index);
                }
            }
        }
    }

    fn show_settings_dialog(&self) {
        #[cfg(target_os = "macos")]
        {
            use objc2::msg_send;
            use objc2_app_kit::NSWindow;
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

            let mut parent_ptr: *const NSWindow = std::ptr::null();
            if let Some(win) = &self.window
                && let Ok(RawWindowHandle::AppKit(handle)) = win.window_handle().map(|h| h.as_raw())
            {
                let ns_view = handle.ns_view.as_ptr() as *const objc2::runtime::AnyObject;
                let ns_win: *const NSWindow = unsafe { msg_send![ns_view, window] };
                if !ns_win.is_null() {
                    parent_ptr = ns_win;
                }
            }

            native_ui::show_settings_window(parent_ptr);
        }
    }

    fn show_about_dialog(&self) {
        #[cfg(target_os = "macos")]
        {
            use objc2::msg_send;
            use objc2_app_kit::NSWindow;
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

            let mut parent_ptr: *const NSWindow = std::ptr::null();
            if let Some(win) = &self.window
                && let Ok(RawWindowHandle::AppKit(handle)) = win.window_handle().map(|h| h.as_raw())
            {
                let ns_view = handle.ns_view.as_ptr() as *const objc2::runtime::AnyObject;
                let ns_win: *const NSWindow = unsafe { msg_send![ns_view, window] };
                if !ns_win.is_null() {
                    parent_ptr = ns_win;
                }
            }

            native_ui::show_about_window(parent_ptr);
        }
    }

    fn handle_menu_event(&mut self) {
        let Some(app_menu) = &self.app_menu else {
            return;
        };
        let Some(event) = menu::poll_menu_event() else {
            return;
        };

        // CheckMenuItems auto-toggle on click, so we read their new state directly
        if event.id() == &app_menu.ids.auto_fit_window {
            let enabled = app_menu.auto_fit_item.is_checked();
            log::debug!("Menu: Auto-fit window -> {enabled}");
            let _ = self
                .event_loop_proxy
                .send_event(AppCommand::SetAutoFitWindow(enabled));
            return;
        }
        if event.id() == &app_menu.ids.enlarge_small_images {
            let enabled = app_menu.enlarge_small_item.is_checked();
            log::debug!("Menu: Enlarge small images -> {enabled}");
            let _ = self
                .event_loop_proxy
                .send_event(AppCommand::SetEnlargeSmallImages(enabled));
            return;
        }

        if let Some(command) = input::menu_to_command(&event, &app_menu.ids) {
            log::debug!("Menu event: {:?}", event.id());
            let _ = self.event_loop_proxy.send_event(command);
        } else {
            log::debug!("Menu: unhandled event {:?}", event.id());
        }
    }

    /// Push current app state into the shared mutex for the QA server to read.
    fn update_shared_state(&self) {
        let Ok(mut state) = self.shared_state.lock() else {
            return;
        };

        state.zoom = self.view_state.zoom;
        state.pan_x = self.view_state.pan_x;
        state.pan_y = self.view_state.pan_y;
        state.auto_fit_window = self.auto_fit_window;
        state.enlarge_small_images = self.enlarge_small_images;

        if let Some(win) = &self.window {
            let size = win.inner_size();
            state.window_width = size.width;
            state.window_height = size.height;
            state.fullscreen = window::is_fullscreen(win);
            state.window_title = win.title();
        }

        if let Some(dir) = &self.dir_list {
            state.current_file = Some(dir.current().to_path_buf());
            state.current_index = dir.current_index();
            state.total_files = dir.len();
        }

        if let Some((iw, ih)) = self.current_image_size {
            state.image_width = iw;
            state.image_height = ih;
        }
        state.min_zoom = self.view_state.min_zoom_value();
        let (rx, ry, rw, rh) = self.view_state.rendered_rect();
        state.image_render_x = rx;
        state.image_render_y = ry;
        state.image_render_width = rw;
        state.image_render_height = rh;

        state.diagnostics_text = self.build_diagnostics_text(state.current_index);
    }

    /// Build human/agent-readable diagnostics text covering cache, navigation timing, and memory.
    fn build_diagnostics_text(&self, current_index: usize) -> String {
        let mut out = String::new();

        // Cache diagnostics
        let cache_diag = self.image_cache.diagnostics();
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
                    "  ← current"
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

        // Preloader status
        out.push_str("\npreloader:\n");
        out.push_str(&format!(
            "  window: current ± {}\n",
            preloader::preload_count()
        ));

        // Navigation history
        out.push_str("\nrecent_navigations (newest first):\n");
        if self.navigation_history.is_empty() {
            out.push_str("  (none)\n");
        } else {
            let now = Instant::now();
            for record in self.navigation_history.iter().rev() {
                let ago = now.duration_since(record.timestamp);
                let cached_str = if record.was_cached { "yes" } else { "no " };
                out.push_str(&format!(
                    "  {}→{}  cached: {}  display: {}ms  {:.1}s ago\n",
                    record.from_index,
                    record.to_index,
                    cached_str,
                    record.total_time.as_millis(),
                    ago.as_secs_f64(),
                ));
            }
        }

        // Process memory via ps
        let process_memory = get_process_rss_mb();
        out.push_str(&format!(
            "\nprocess_memory: {:.1} MB (cache: {})\n",
            process_memory,
            format_bytes(cache_diag.total_memory)
        ));

        out
    }

    /// Central command executor. All user actions — keyboard, mouse, menu, QA server —
    /// are mapped to `AppCommand` and dispatched here.
    fn execute_command(&mut self, event_loop: &ActiveEventLoop, command: AppCommand) {
        match command {
            AppCommand::SendKey(key_name) => {
                if let Some(cmd) = input::qa_key_to_command(&key_name) {
                    self.execute_command(event_loop, cmd);
                }
            }
            AppCommand::Navigate(forward) => self.navigate(forward),
            AppCommand::ZoomIn => {
                self.view_state.keyboard_zoom(true);
                self.update_transform_and_redraw();
            }
            AppCommand::ZoomOut => {
                self.view_state.keyboard_zoom(false);
                self.update_transform_and_redraw();
            }
            AppCommand::SetZoom(level) => {
                self.view_state.set_zoom(level);
                self.update_transform_and_redraw();
            }
            AppCommand::FitToWindow => {
                self.view_state.fit_to_window();
                self.update_transform_and_redraw();
            }
            AppCommand::ActualSize => {
                self.view_state.actual_size();
                self.update_transform_and_redraw();
            }
            AppCommand::ToggleFit => {
                self.view_state.toggle_fit();
                self.update_transform_and_redraw();
            }
            AppCommand::ToggleFullscreen => {
                if let Some(win) = &self.window {
                    window::toggle_fullscreen(win);
                    self.update_shared_state();
                }
            }
            AppCommand::SetFullscreen(on) => {
                if let Some(win) = &self.window {
                    window::set_fullscreen(win, on);
                    self.update_shared_state();
                }
            }
            AppCommand::SetAutoFitWindow(enabled) => {
                self.auto_fit_window = enabled;
                log::debug!("Auto-fit window set to: {enabled}");
                let mut s = settings::Settings::load();
                s.auto_fit_window = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.auto_fit_item.set_checked(enabled);
                    // "Enlarge small images" is irrelevant when auto-fit is on
                    menu.enlarge_small_item.set_enabled(!enabled);
                }
                if enabled
                    && let (Some(win), Some((iw, ih))) = (&self.window, self.current_image_size)
                {
                    window::resize_to_fit_image(win, iw, ih);
                }
                // Re-apply zoom: auto-fit changes whether min_zoom can go below 1.0
                self.apply_initial_zoom();
                self.update_transform_and_redraw();
            }
            AppCommand::SetEnlargeSmallImages(enabled) => {
                self.enlarge_small_images = enabled;
                log::debug!("Enlarge small images set to: {enabled}");
                let mut s = settings::Settings::load();
                s.enlarge_small_images = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.enlarge_small_item.set_checked(enabled);
                }
                // Re-apply zoom: toggling this changes whether small images enlarge or not
                self.apply_initial_zoom();
                self.update_transform_and_redraw();
            }
            AppCommand::ShowAbout => self.show_about_dialog(),
            AppCommand::ShowSettings => self.show_settings_dialog(),
            AppCommand::Exit => {
                // Escape exits fullscreen first, then exits the app
                if let Some(win) = &self.window
                    && window::is_fullscreen(win)
                {
                    log::info!("Fullscreen off");
                    window::set_fullscreen(win, false);
                    self.update_shared_state();
                    return;
                }
                log::info!("Exiting");
                if let Some(preloader) = self.preloader.take() {
                    preloader.shutdown();
                }
                event_loop.exit();
            }
            AppCommand::OpenFile(path) => {
                let resolved = path.canonicalize().unwrap_or(path);
                if resolved.is_file() {
                    self.file_path = resolved.clone();
                    self.dir_list = directory::DirectoryList::from_file(&resolved);
                    self.display_image(&resolved);

                    if let Some(dir) = &self.dir_list
                        && let Some(win) = &self.window
                    {
                        win.set_title(&window::window_title_with_position(
                            &resolved,
                            dir.current_index(),
                            dir.len(),
                        ));
                    }

                    self.update_shared_state();
                } else {
                    log::warn!("OpenFile: not a file: {}", resolved.display());
                }
            }
            AppCommand::SetWindowGeometry {
                x,
                y,
                width,
                height,
            } => {
                if let Some(win) = &self.window {
                    if let Some(w) = width
                        && let Some(h) = height
                    {
                        let _ = win
                            .request_inner_size(winit::dpi::LogicalSize::new(w as f64, h as f64));
                    }
                    if x.is_some() || y.is_some() {
                        let current = win.outer_position().unwrap_or_default();
                        let new_x = x.unwrap_or(current.x);
                        let new_y = y.unwrap_or(current.y);
                        win.set_outer_position(winit::dpi::LogicalPosition::new(
                            new_x as f64,
                            new_y as f64,
                        ));
                    }
                    if let Some(renderer) = &mut self.renderer {
                        let size = win.inner_size();
                        renderer.resize(size.width, size.height);
                        if let Some((iw, ih)) = self.current_image_size {
                            self.view_state
                                .update_dimensions(iw, ih, size.width, size.height);
                        }
                    }
                    self.update_min_zoom();
                    if let Some(renderer) = &self.renderer {
                        renderer.update_transform(&self.view_state.transform());
                    }
                    self.request_redraw();
                    self.update_shared_state();
                }
            }
            AppCommand::ScrollZoom {
                delta,
                cursor_x,
                cursor_y,
            } => {
                self.view_state.scroll_zoom(delta, cursor_x, cursor_y);
                self.update_transform_and_redraw();
            }
            AppCommand::TakeScreenshot(sender) => {
                let png_bytes = if let Some(renderer) = &self.renderer {
                    renderer.capture_screenshot()
                } else {
                    Vec::new()
                };
                let _ = sender.send(png_bytes);
            }
        }
    }
}

impl ApplicationHandler<AppCommand> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Already initialized
        }

        // Register the event loop proxy globally so native UI delegates can send commands
        qa_server::set_event_loop_proxy(self.event_loop_proxy.clone());

        // Create window
        let win = window::create_window(event_loop, &self.file_path);
        self.window = Some(win.clone());

        // Create renderer (wgpu surface must be created here, in resumed())
        self.renderer = Some(renderer::Renderer::new(win));

        // Create native menu bar
        // The AppMenu MUST be stored (not dropped) to keep the MenuChild backing data alive.
        // Dropping it frees the native menu items' backing memory, causing a crash on click.
        self.app_menu = Some(menu::create_menu_bar());

        // Build the navigation list: explicit file list (multi-select) or directory scan
        self.dir_list = if let Some(files) = self.explicit_files.take() {
            Some(directory::DirectoryList::from_explicit(files))
        } else {
            directory::DirectoryList::from_file(&self.file_path)
        };

        // Start preloader thread pool
        let mut preloader = preloader::Preloader::start();

        // Load and display the initial image
        let initial_path = self.file_path.clone();
        self.display_image(&initial_path);

        // Cache the initial image and request preloading of adjacent images
        if let Some(dir) = &self.dir_list {
            let current_index = dir.current_index();
            let total = dir.len();

            // Update window title with position
            if let Some(win) = &self.window {
                win.set_title(&window::window_title_with_position(
                    &self.file_path,
                    current_index,
                    total,
                ));
            }

            // Request preloading of adjacent images
            let to_preload: Vec<(usize, PathBuf)> = dir
                .preload_range(preloader::preload_count())
                .iter()
                .filter_map(|&i| dir.get(i).map(|p| (i, p.to_path_buf())))
                .collect();

            if !to_preload.is_empty() {
                preloader.request_preload(to_preload);
            }
        }

        self.preloader = Some(preloader);

        // Populate shared state before starting the QA server
        self.update_shared_state();

        // Start the QA HTTP server
        self._qa_handle = qa_server::start(
            Arc::clone(&self.shared_state),
            self.event_loop_proxy.clone(),
        );

        // Check for updates in the background (if enabled in settings)
        #[cfg(target_os = "macos")]
        if settings::Settings::load().auto_update {
            updater::check_and_update();
        }

        // Force a redraw after everything is initialized. The initial display_image() call
        // may have been skipped because the surface was Occluded during window creation.
        self.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, command: AppCommand) {
        self.execute_command(event_loop, command);
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Poll menu events and preloader on every event loop iteration, not just window events.
        // Without this, menu clicks would only be processed when the next window event fires
        // (mouse move, key press, etc.), causing multi-second delays.
        self.poll_preloader();
        self.handle_menu_event();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        self.poll_preloader();

        match event {
            WindowEvent::CloseRequested => {
                log::info!("Exiting (window closed)");
                if let Some(preloader) = self.preloader.take() {
                    preloader.shutdown();
                }
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                log::debug!("Window resized to {}x{}", size.width, size.height);
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                    if let Some((iw, ih)) = self.current_image_size {
                        self.view_state
                            .update_dimensions(iw, ih, size.width, size.height);
                    }
                }
                // Recalculate zoom floor — image-to-window ratio changed
                self.update_min_zoom();
                if let Some(renderer) = &self.renderer {
                    renderer.update_transform(&self.view_state.transform());
                }
                self.request_redraw();
                self.update_shared_state();
            }

            WindowEvent::RedrawRequested => {
                if self.needs_redraw {
                    log::trace!("Rendering frame");
                    let text_blocks = self.build_text_overlay();
                    let rendered = self
                        .renderer
                        .as_mut()
                        .is_some_and(|renderer| renderer.render(&text_blocks));
                    if rendered {
                        self.needs_redraw = false;
                    } else {
                        if let Some(win) = &self.window {
                            win.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(command) =
                    input::key_to_command(event.logical_key.as_ref(), &self.modifiers)
                {
                    self.execute_command(event_loop, command);
                }
            }

            // Scroll zoom: cursor-centered, not a discrete command
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll_y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / 50.0,
                };
                if scroll_y.abs() > f32::EPSILON {
                    let (cx, cy) = self.last_mouse_pos;
                    self.view_state.scroll_zoom(scroll_y, cx as f32, cy as f32);
                    self.update_transform_and_redraw();
                }
            }

            // Mouse drag for panning
            WindowEvent::CursorMoved { position, .. } => {
                let prev = self.last_mouse_pos;
                self.last_mouse_pos = (position.x, position.y);

                if self.drag_start.is_some() {
                    let dx = position.x - prev.0;
                    let dy = position.y - prev.1;
                    self.view_state.pan(dx as f32, dy as f32);
                    self.update_transform_and_redraw();
                }
            }

            // Click / double-click / drag tracking
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    let now = Instant::now();
                    if let Some(last) = self.last_click_time
                        && now.duration_since(last).as_millis() < 400
                    {
                        self.execute_command(event_loop, AppCommand::ToggleFit);
                        self.last_click_time = None;
                        self.drag_start = None;
                        return;
                    }
                    self.last_click_time = Some(now);
                    self.drag_start = Some(self.last_mouse_pos);
                }
                ElementState::Released => {
                    self.drag_start = None;
                }
            },

            _ => {}
        }
    }
}

/// Format a byte count as a human-readable string (for example, "47.2 MB").
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

/// Get the current process RSS in MB via `ps`. Returns 0.0 on failure.
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
