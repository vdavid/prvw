mod directory;
mod image_loader;
#[cfg(target_os = "macos")]
mod macos_open_handler;
mod menu;
mod preloader;
mod qa_server;
mod renderer;
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
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

#[derive(Parser)]
#[command(name = "prvw", about = "A fast, minimal image viewer")]
struct Cli {
    /// Path(s) to image file(s) to open
    #[arg(required = true)]
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

    if resolved_files.is_empty() {
        eprintln!("No valid image files provided");
        std::process::exit(1);
    }

    if resolved_files.len() == 1 {
        log::info!("Opening {}", resolved_files[0].display());
    } else {
        log::info!("Opening {} files", resolved_files.len());
    }

    let file_path = resolved_files[0].clone();

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
}

impl App {
    fn new(
        file_path: PathBuf,
        explicit_files: Option<Vec<PathBuf>>,
        event_loop_proxy: EventLoopProxy<AppCommand>,
        shared_state: Arc<Mutex<SharedAppState>>,
    ) -> Self {
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
        }
    }

    /// Load and display an image, updating the renderer and view state.
    fn display_image(&mut self, path: &Path) {
        let renderer = match &mut self.renderer {
            Some(r) => r,
            None => return,
        };

        let filename = path.file_name().unwrap_or_default().to_string_lossy();

        match image_loader::load_image(path) {
            Ok(image) => {
                self.current_image_size = Some((image.width, image.height));
                self.view_state.update_dimensions(
                    image.width,
                    image.height,
                    renderer.surface_width(),
                    renderer.surface_height(),
                );
                self.view_state.fit_to_window();
                renderer.set_image(&image);
                renderer.update_transform(&self.view_state.transform());
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
        let renderer = match &mut self.renderer {
            Some(r) => r,
            None => return,
        };

        if let Some(image) = self.image_cache.get(index) {
            self.current_image_size = Some((image.width, image.height));
            self.view_state.update_dimensions(
                image.width,
                image.height,
                renderer.surface_width(),
                renderer.surface_height(),
            );
            self.view_state.fit_to_window();
            renderer.set_image(image);
            renderer.update_transform(&self.view_state.transform());
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

    fn show_about_dialog(&self) {
        let version = env!("CARGO_PKG_VERSION");
        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            let message = format!(
                "Prvw {version}\\n\\nA fast image viewer for macOS.\\n\\nBy David Veszelovszki\\nhttps://veszelovszki.com\\n\\nhttps://getprvw.com"
            );
            let _ = Command::new("osascript")
                .args([
                    "-e",
                    &format!(
                        "display dialog \"{message}\" with title \"About Prvw\" buttons {{\"OK\"}} default button \"OK\" with icon note"
                    ),
                ])
                .spawn();
        }
    }

    fn handle_menu_event(&mut self) {
        let Some(app_menu) = &self.app_menu else {
            return;
        };
        let ids = &app_menu.ids;
        let Some(event) = menu::poll_menu_event() else {
            return;
        };

        log::debug!("Menu event received: {:?}", event.id());

        if event.id() == &ids.about {
            log::debug!("Menu: About");
            self.show_about_dialog();
        } else if event.id() == &ids.zoom_in {
            log::debug!("Menu: Zoom in");
            self.view_state.keyboard_zoom(true);
            self.update_transform_and_redraw();
        } else if event.id() == &ids.zoom_out {
            log::debug!("Menu: Zoom out");
            self.view_state.keyboard_zoom(false);
            self.update_transform_and_redraw();
        } else if event.id() == &ids.actual_size {
            log::debug!("Menu: Actual size");
            self.view_state.actual_size();
            self.update_transform_and_redraw();
        } else if event.id() == &ids.fit_to_window {
            log::debug!("Menu: Fit to window");
            self.view_state.fit_to_window();
            self.update_transform_and_redraw();
        } else if event.id() == &ids.fullscreen {
            log::debug!("Menu: Fullscreen");
            if let Some(win) = &self.window {
                window::toggle_fullscreen(win);
                self.update_shared_state();
            }
        } else if event.id() == &ids.previous {
            log::debug!("Menu: Previous");
            self.navigate(false);
        } else if event.id() == &ids.next {
            log::debug!("Menu: Next");
            self.navigate(true);
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

    /// Handle a key name from the QA server (web-style key names).
    fn handle_qa_key(&mut self, event_loop: &ActiveEventLoop, key_name: &str) {
        match key_name {
            "ArrowLeft" | "Backspace" | "[" => self.navigate(false),
            "ArrowRight" | " " | "Space" | "]" => self.navigate(true),
            "Enter" => {
                if let Some(win) = &self.window {
                    window::toggle_fullscreen(win);
                    self.update_shared_state();
                }
            }
            "Escape" => {
                if let Some(win) = &self.window {
                    if window::is_fullscreen(win) {
                        log::info!("Fullscreen off");
                        window::toggle_fullscreen(win);
                        self.update_shared_state();
                    } else {
                        log::info!("Exiting (Escape via QA)");
                        if let Some(preloader) = self.preloader.take() {
                            preloader.shutdown();
                        }
                        event_loop.exit();
                    }
                }
            }
            "F11" => {
                if let Some(win) = &self.window {
                    window::toggle_fullscreen(win);
                    self.update_shared_state();
                }
            }
            "f" => {
                // Cmd+F equivalent: toggle fullscreen
                if let Some(win) = &self.window {
                    window::toggle_fullscreen(win);
                    self.update_shared_state();
                }
            }
            "+" | "=" => {
                self.view_state.keyboard_zoom(true);
                self.update_transform_and_redraw();
            }
            "-" => {
                self.view_state.keyboard_zoom(false);
                self.update_transform_and_redraw();
            }
            "0" => {
                self.view_state.fit_to_window();
                self.update_transform_and_redraw();
            }
            "1" => {
                self.view_state.actual_size();
                self.update_transform_and_redraw();
            }
            _ => {
                log::debug!("QA server: unhandled key '{key_name}'");
            }
        }
    }
}

impl ApplicationHandler<AppCommand> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Already initialized
        }

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

        // Force a redraw after everything is initialized. The initial display_image() call
        // may have been skipped because the surface was Occluded during window creation.
        self.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, command: AppCommand) {
        match command {
            AppCommand::SendKey(key_name) => {
                self.handle_qa_key(event_loop, &key_name);
            }
            AppCommand::Navigate(forward) => {
                self.navigate(forward);
            }
            AppCommand::SetZoom(level) => {
                self.view_state.zoom = level;
                self.view_state.pan_x = 0.0;
                self.view_state.pan_y = 0.0;
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
                    log::warn!("QA /open: not a file: {}", resolved.display());
                }
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
                    renderer.update_transform(&self.view_state.transform());
                }
                self.request_redraw();
                self.update_shared_state();
            }

            WindowEvent::RedrawRequested => {
                if self.needs_redraw {
                    log::trace!("Rendering frame");
                    let rendered = self
                        .renderer
                        .as_ref()
                        .is_some_and(|renderer| renderer.render());
                    if rendered {
                        self.needs_redraw = false;
                    } else {
                        // Surface wasn't ready (Occluded, Lost). Try again next frame.
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
                let super_pressed = self.modifiers.super_key();
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => {
                        if let Some(win) = &self.window {
                            if window::is_fullscreen(win) {
                                log::info!("Fullscreen off");
                                window::toggle_fullscreen(win);
                                self.update_shared_state();
                            } else {
                                log::info!("Exiting (Escape)");
                                if let Some(preloader) = self.preloader.take() {
                                    preloader.shutdown();
                                }
                                event_loop.exit();
                            }
                        }
                    }
                    // Navigation: ←, →, Space, Backspace, [, ]
                    Key::Named(NamedKey::ArrowLeft)
                    | Key::Named(NamedKey::Backspace)
                    | Key::Character("[") => self.navigate(false),
                    Key::Named(NamedKey::ArrowRight)
                    | Key::Named(NamedKey::Space)
                    | Key::Character("]") => self.navigate(true),
                    // Fullscreen: F, Cmd+F, Enter, Opt+Enter, F11
                    Key::Named(NamedKey::F11)
                    | Key::Named(NamedKey::Enter)
                    | Key::Character("f") => {
                        if let Some(win) = &self.window {
                            let entering = !window::is_fullscreen(win);
                            log::info!("Fullscreen {}", if entering { "on" } else { "off" });
                            window::toggle_fullscreen(win);
                            self.update_shared_state();
                        }
                    }
                    Key::Character("=") | Key::Character("+") if super_pressed => {
                        self.view_state.keyboard_zoom(true);
                        self.update_transform_and_redraw();
                    }
                    Key::Character("-") if super_pressed => {
                        self.view_state.keyboard_zoom(false);
                        self.update_transform_and_redraw();
                    }
                    Key::Character("=") | Key::Character("+") => {
                        self.view_state.keyboard_zoom(true);
                        self.update_transform_and_redraw();
                    }
                    Key::Character("-") => {
                        self.view_state.keyboard_zoom(false);
                        self.update_transform_and_redraw();
                    }
                    Key::Character("0") => {
                        self.view_state.fit_to_window();
                        self.update_transform_and_redraw();
                    }
                    Key::Character("1") => {
                        self.view_state.actual_size();
                        self.update_transform_and_redraw();
                    }
                    _ => {}
                }
            }

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
                        // Double-click: toggle fit-to-window vs actual size
                        self.view_state.toggle_fit();
                        self.update_transform_and_redraw();
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
