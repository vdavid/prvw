mod color;
mod directory;
#[cfg(target_os = "macos")]
mod display_profile;
mod image_loader;
mod input;
#[cfg(target_os = "macos")]
mod macos_open_handler;
mod menu;
#[cfg(target_os = "macos")]
mod native_ui;
#[cfg(target_os = "macos")]
mod onboarding;
mod pixels;
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
use pixels::{
    Logical, from_logical_pos, from_logical_size, from_physical_size, to_logical_pos,
    to_logical_size,
};
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

    let waiting_for_file = resolved_files.is_empty();

    if waiting_for_file {
        log::info!("No files on CLI, waiting for Apple Event (Finder double-click)");
    } else if resolved_files.len() == 1 {
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

    // Inject application:openURLs: into winit's delegate class so macOS routes file-open
    // events to us instead of NSDocumentController (which shows "cannot open files" errors).
    // Must happen after EventLoop::new() (which creates the WinitApplicationDelegate class)
    // but before run_app() (which calls finishLaunching and dispatches queued Apple Events).
    #[cfg(target_os = "macos")]
    {
        macos_open_handler::set_proxy(proxy.clone());
        macos_open_handler::register();
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
    drag_start: Option<(Logical<f64>, Logical<f64>)>,
    last_mouse_pos: (Logical<f64>, Logical<f64>),
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
    /// Whether ICC color management is enabled (Level 1: source -> sRGB).
    icc_color_management: bool,
    /// Whether to use the display's ICC profile (Level 2) or sRGB (Level 1).
    color_match_display: bool,
    /// Whether to use relative colorimetric rendering intent instead of perceptual.
    use_relative_colorimetric: bool,
    /// Current display scale factor (Retina = 2.0). Updated on window creation and
    /// `ScaleFactorChanged` events. Defaults to 2.0 before the window exists.
    scale_factor: f64,
    /// True when launched with no CLI files (Finder double-click or Dock launch).
    /// The app waits for an Apple Event before creating the main window.
    waiting_for_file: bool,
    /// When waiting_for_file: the time we started waiting. After 500ms with no file,
    /// show the onboarding window.
    wait_start: Option<Instant>,
    /// ICC profile bytes for the current display (target color space for image decoding).
    /// Defaults to system sRGB; updated when the display is detected or the window moves.
    display_icc: Vec<u8>,
}

impl App {
    fn new(
        file_path: PathBuf,
        explicit_files: Option<Vec<PathBuf>>,
        waiting_for_file: bool,
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
            last_mouse_pos: (Logical(0.0), Logical(0.0)),
            last_click_time: None,
            needs_redraw: false,
            shared_state,
            event_loop_proxy,
            _qa_handle: None,
            navigation_history: VecDeque::with_capacity(10),
            current_image_size: None,
            auto_fit_window: initial_settings.auto_fit_window,
            enlarge_small_images: initial_settings.enlarge_small_images,
            icc_color_management: initial_settings.icc_color_management,
            color_match_display: initial_settings.color_match_display,
            use_relative_colorimetric: initial_settings.use_relative_colorimetric,
            scale_factor: 2.0,
            waiting_for_file,
            wait_start: None,
            display_icc: color::srgb_icc_bytes().to_vec(),
        }
    }

    /// Recalculate the zoom floor based on current image/window/settings state.
    /// Called on image load, window resize, and setting changes. Does NOT change the
    /// current zoom level (only reclamps if it's below the new floor).
    fn update_min_zoom(&mut self) {
        if self.auto_fit_window {
            // With auto-fit, the window tracks zoom. The floor is the zoom that would
            // make the window hit the minimum size (200px logical per axis).
            if let Some((iw, ih)) = self.current_image_size {
                let max_dim = iw.max(ih) as f64;
                self.view_state
                    .set_min_zoom((window::MIN_WINDOW_DIM / max_dim) as f32);
            }
            return;
        }

        let fit = self.view_state.fit_zoom();
        let is_small = fit > 1.0;
        if is_small && !self.enlarge_small_images {
            self.view_state.set_min_zoom(1.0);
        } else {
            self.view_state.set_min_zoom(fit);
        }
    }

    /// Compute the target ICC bytes based on current settings.
    /// - ICC off: empty (no transforms)
    /// - ICC on, color match off: sRGB (Level 1)
    /// - ICC on, color match on: display profile (Level 2)
    fn effective_display_icc(&self, window: &Window) -> Vec<u8> {
        if !self.icc_color_management {
            return Vec::new(); // No ICC transforms
        }
        #[cfg(target_os = "macos")]
        if self.color_match_display
            && let Some(icc) = display_profile::get_display_icc(window)
        {
            return icc;
        }
        // Suppress unused variable warning on non-macOS
        let _ = window;
        color::srgb_icc_bytes().to_vec()
    }

    /// Window center in logical pixels (for auto-fit pivot when zooming via keyboard/menu).
    fn window_center_logical(&self) -> (Logical<f64>, Logical<f64>) {
        self.window
            .as_ref()
            .map(|w| {
                let (lw, lh) =
                    from_logical_size(w.inner_size().to_logical::<f64>(w.scale_factor()));
                (lw * 0.5, lh * 0.5)
            })
            .unwrap_or((Logical(0.0), Logical(0.0)))
    }

    /// After a zoom change with auto-fit ON, resize the window to match the zoomed image.
    /// `pivot_win_x/y` is the cursor position in logical window pixels — the screen pixel under
    /// the cursor should stay over the same image content after the resize.
    fn auto_fit_after_zoom(
        &mut self,
        old_zoom: f32,
        pivot_win_x: Logical<f64>,
        pivot_win_y: Logical<f64>,
    ) {
        let Some((iw, ih)) = self.current_image_size else {
            return;
        };
        let Some(win) = &self.window else {
            return;
        };
        if window::is_fullscreen(win) {
            return;
        }

        let new_zoom = self.view_state.zoom;
        let scale = win.scale_factor();

        // Desired window = image * zoom (already in logical pixels with the new zoom model)
        let desired_w = iw as f64 * new_zoom as f64;
        let desired_h = ih as f64 * new_zoom as f64;

        // Cap at screen bounds, floor at minimum
        let monitor_bounds = window::MonitorBounds::from_window(win);
        let (max_w, max_h) = monitor_bounds
            .as_ref()
            .map(|b| {
                let (w, h) = b.max_window_size();
                (w.0, h.0)
            })
            .unwrap_or((desired_w, desired_h));

        let final_w = desired_w.clamp(window::MIN_WINDOW_DIM, max_w);
        let final_h = desired_h.clamp(window::MIN_WINDOW_DIM, max_h);

        // Check if the window can fully accommodate the zoomed image (no capping).
        // If capped, the existing pan from scroll_zoom handles the overflow — don't reposition.
        let fully_fits = (final_w - desired_w).abs() < 1.0 && (final_h - desired_h).abs() < 1.0;

        if fully_fits {
            // Pan is unnecessary — image fills the window exactly
            self.view_state.pan_x = 0.0;
            self.view_state.pan_y = 0.0;
        }

        let (win_pos_x, win_pos_y) = from_logical_pos(
            win.outer_position()
                .unwrap_or_default()
                .to_logical::<f64>(scale),
        );
        // Position math uses outer_position, so we need outer dimensions.
        // The titlebar adds height to the outer frame vs the inner content area.
        let (outer_w, outer_h) = from_logical_size(win.outer_size().to_logical::<f64>(scale));
        let (inner_w, inner_h) = from_logical_size(win.inner_size().to_logical::<f64>(scale));
        let chrome_w = outer_w - inner_w; // typically 0 on macOS
        let chrome_h = outer_h - inner_h; // titlebar height

        // The new outer size after request_inner_size(final_w, final_h)
        let new_outer_w = Logical(final_w) + chrome_w;
        let new_outer_h = Logical(final_h) + chrome_h;

        // If the window size isn't changing, skip entirely to avoid sub-pixel drift from
        // rounding between logical/physical coordinates.
        if (new_outer_w - outer_w).0.abs() < 1.5 && (new_outer_h - outer_h).0.abs() < 1.5 {
            return;
        }

        let growing = new_outer_w.0 > outer_w.0 + 0.5 || new_outer_h.0 > outer_h.0 + 0.5;

        // Positioning strategy:
        // - Growing: use pivot (keeps cursor over the same image content — feels natural)
        // - Shrinking or same size: center the reduction (stable, no drift)
        let (target_x, target_y) = if growing {
            // Pivot: the cursor's screen position should stay over the same image content.
            // The pivot is in logical window pixels.
            // Add chrome_h to pivot_y because outer_position.y is the frame top, but
            // the cursor is relative to the content area (below the titlebar).
            let screen_x = win_pos_x + pivot_win_x;
            let screen_y = win_pos_y + chrome_h + pivot_win_y;
            let ratio = new_zoom as f64 / old_zoom as f64;
            (
                screen_x - pivot_win_x * ratio,
                screen_y - (chrome_h + pivot_win_y) * ratio,
            )
        } else {
            // Shrink symmetrically around the window center (outer frame center)
            (
                win_pos_x + (outer_w - new_outer_w) * 0.5,
                win_pos_y + (outer_h - new_outer_h) * 0.5,
            )
        };

        // Screen boundary: the window must not go MORE off-screen than it was before.
        let (final_x, final_y) = if let Some(bounds) = &monitor_bounds {
            window::clamp_to_screen(
                (target_x, target_y),
                (new_outer_w, new_outer_h),
                (win_pos_x, win_pos_y),
                (outer_w, outer_h),
                bounds,
            )
        } else {
            (target_x, target_y)
        };

        let new_size = to_logical_size(Logical(final_w), Logical(final_h));
        let (pw, ph) = from_physical_size(new_size.to_physical::<u32>(scale));
        let _ = win.request_inner_size(new_size);
        win.set_outer_position(to_logical_pos(final_x, final_y));

        // Update renderer with the new size immediately (request_inner_size is async)
        if let Some(renderer) = &mut self.renderer {
            renderer.resize(pw, ph);
            if let Some((iw, ih)) = self.current_image_size {
                self.view_state.update_dimensions(
                    iw,
                    ih,
                    renderer.logical_width(),
                    renderer.logical_height(),
                );
            }
            renderer.update_transform(&self.view_state.transform());
        }
    }

    /// Choose the right initial zoom for a newly loaded image.
    /// Sets both the zoom floor and the starting zoom level.
    fn apply_initial_zoom(&mut self) {
        self.update_min_zoom();
        let fit = self.view_state.fit_zoom();
        let is_small = fit > 1.0;

        if is_small && !self.enlarge_small_images && !self.auto_fit_window {
            self.view_state.actual_size(); // show at native pixel size
        } else {
            self.view_state.fit_to_window(); // fill the window
        }
    }

    /// Initialize the full viewer: window, renderer, menu, preloader, initial image.
    /// Called from resumed() (CLI files) or OpenFile handler (Apple Event after waiting).
    fn initialize_viewer(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // Already initialized
        }

        event_loop.set_control_flow(ControlFlow::Wait);

        // Create window
        let win = window::create_window(event_loop, &self.file_path);
        self.scale_factor = win.scale_factor();
        self.window = Some(win.clone());

        // Create renderer (wgpu surface must be created here, in resumed())
        self.renderer = Some(renderer::Renderer::new(win.clone()));

        // Configure ICC color management based on settings
        self.display_icc = self.effective_display_icc(&win);
        #[cfg(target_os = "macos")]
        {
            if !self.display_icc.is_empty() {
                display_profile::set_layer_colorspace(&win, &self.display_icc);
            }
            display_profile::register_screen_change_observer(&win);
        }

        // Create native menu bar
        self.app_menu = Some(menu::create_menu_bar());

        // Build the navigation list
        self.dir_list = if let Some(files) = self.explicit_files.take() {
            Some(directory::DirectoryList::from_explicit(files))
        } else {
            directory::DirectoryList::from_file(&self.file_path)
        };

        // Start preloader thread pool
        let mut preloader =
            preloader::Preloader::start(self.display_icc.clone(), self.use_relative_colorimetric);

        // Load and display the initial image
        let initial_path = self.file_path.clone();
        self.display_image(&initial_path);

        if let Some(dir) = &self.dir_list {
            let current_index = dir.current_index();
            let total = dir.len();

            if let Some(win) = &self.window {
                win.set_title(&window::window_title_with_position(
                    &self.file_path,
                    current_index,
                    total,
                ));
            }

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
        self.update_shared_state();

        // Start QA server if not already running (it starts early when waiting_for_file)
        if self._qa_handle.is_none() {
            self._qa_handle = qa_server::start(
                Arc::clone(&self.shared_state),
                self.event_loop_proxy.clone(),
            );
        }

        #[cfg(target_os = "macos")]
        if settings::Settings::load().auto_update {
            updater::check_and_update();
        }

        self.request_redraw();
    }

    /// Load and display an image, updating the renderer and view state.
    fn display_image(&mut self, path: &Path) {
        if self.renderer.is_none() {
            return;
        }

        let filename = path.file_name().unwrap_or_default().to_string_lossy();

        match image_loader::load_image(path, &self.display_icc, self.use_relative_colorimetric) {
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
                    let (pw, ph) = from_physical_size(size);
                    renderer.resize(pw, ph);
                }

                self.view_state.update_dimensions(
                    image.width,
                    image.height,
                    renderer.logical_width(),
                    renderer.logical_height(),
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
                let (pw, ph) = from_physical_size(size);
                renderer.resize(pw, ph);
            }

            self.view_state.update_dimensions(
                image.width,
                image.height,
                renderer.logical_width(),
                renderer.logical_height(),
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

    /// Build text blocks for the header overlay. Pills are computed from actual text
    /// measurements during prepare() — no manual rect computation needed here.
    fn build_text_overlay(&self) -> Vec<text::TextBlock> {
        let Some(rend) = &self.renderer else {
            return Vec::new();
        };
        let Some(dir) = &self.dir_list else {
            return Vec::new();
        };

        let logical_width = rend.logical_width();

        let filename = dir
            .current()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Prvw");

        let title = if dir.len() > 1 {
            format!(
                "{} / {} \u{2013} {filename}",
                dir.current_index() + 1,
                dir.len()
            )
        } else {
            filename.to_string()
        };

        let zoom_pct = (self.view_state.zoom * 100.0).round() as i32;
        let zoom_text = format!("{zoom_pct}%");

        let pill_color: [f32; 4] = [0.0, 0.0, 0.0, 0.55];
        let pad_x = Logical(8.0_f32);
        let pad_y = Logical(4.0_f32);
        let radius = Logical(5.0_f32);
        let title_x = Logical(80.0_f32); // Right of the traffic lights
        let title_y = Logical(3.0_f32); // Aligned with the native title bar text
        let zoom_margin = Logical(7.0_f32); // Equidistant from top and right edge

        // The zoom pill is right-aligned: x = the right edge of the pill.
        let zoom_right_edge = logical_width - zoom_margin;
        let zoom_budget = Logical(70.0_f32); // space reserved for zoom pill (for title truncation)
        let gap = Logical(12.0_f32); // minimum space between title and zoom pills
        let title_max_render =
            logical_width - title_x - zoom_budget - pad_x * 2.0 - zoom_margin - gap;

        vec![
            // Left: filename with position
            text::TextBlock::new(title, title_x + pad_x, title_y + pad_y)
                .bold()
                .max_render_width(title_max_render)
                .pill(pill_color, pad_x, pad_y, radius),
            // Right: zoom percentage (right-aligned — grows left for larger values)
            text::TextBlock::new(zoom_text, zoom_right_edge, title_y + pad_y)
                .bold()
                .align_right()
                .pill(pill_color, pad_x, pad_y, radius),
        ]
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

    /// Recompute the effective display ICC, update the layer colorspace, flush cache, and re-decode.
    /// Called when either ICC toggle changes.
    fn apply_icc_settings(&mut self) {
        let new_icc = if let Some(win) = &self.window {
            self.effective_display_icc(win)
        } else {
            return;
        };

        if color::profiles_match(&self.display_icc, &new_icc) {
            return; // No change
        }

        self.display_icc = new_icc;

        #[cfg(target_os = "macos")]
        if let Some(win) = &self.window
            && !self.display_icc.is_empty()
        {
            display_profile::set_layer_colorspace(win, &self.display_icc);
        }

        self.image_cache.clear();
        if let Some(preloader) = &mut self.preloader {
            preloader.set_display_icc(self.display_icc.clone());
        }
        if let Some(dir) = &self.dir_list {
            let path = dir.current().to_path_buf();
            self.display_image(&path);
        }
    }

    /// Flush the image cache, update the preloader, and re-decode the current image.
    /// Used when color settings change that don't affect the ICC profile bytes (e.g., rendering intent).
    fn flush_and_redisplay(&mut self) {
        self.image_cache.clear();
        if let Some(preloader) = &mut self.preloader {
            preloader.set_use_relative_colorimetric(self.use_relative_colorimetric);
        }
        if let Some(dir) = &self.dir_list {
            let path = dir.current().to_path_buf();
            self.display_image(&path);
        }
    }

    /// Re-query the display ICC profile and re-decode the current image if the profile changed.
    #[cfg(target_os = "macos")]
    fn handle_display_changed(&mut self) {
        log::debug!("Display changed, re-evaluating ICC settings");
        self.apply_icc_settings();
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
        if event.id() == &app_menu.ids.icc_color_management {
            let enabled = app_menu.icc_color_management_item.is_checked();
            log::debug!("Menu: ICC color management -> {enabled}");
            let _ = self
                .event_loop_proxy
                .send_event(AppCommand::SetIccColorManagement(enabled));
            return;
        }
        if event.id() == &app_menu.ids.color_match_display {
            let enabled = app_menu.color_match_item.is_checked();
            log::debug!("Menu: Color match display -> {enabled}");
            let _ = self
                .event_loop_proxy
                .send_event(AppCommand::SetColorMatchDisplay(enabled));
            return;
        }
        if event.id() == &app_menu.ids.relative_colorimetric {
            let enabled = app_menu.relative_colorimetric_item.is_checked();
            log::debug!("Menu: Relative colorimetric -> {enabled}");
            let _ = self
                .event_loop_proxy
                .send_event(AppCommand::SetRelativeColorimetric(enabled));
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
            let sf = win.scale_factor();
            let (lw, lh) = from_logical_size(win.inner_size().to_logical::<f64>(sf));
            let (lx, ly) = from_logical_pos(
                win.outer_position()
                    .unwrap_or_default()
                    .to_logical::<f64>(sf),
            );
            state.window_x = lx.0;
            state.window_y = ly.0;
            state.window_width = lw.0 as u32;
            state.window_height = lh.0 as u32;
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
        state.image_render_x = rx.0;
        state.image_render_y = ry.0;
        state.image_render_width = rw.0;
        state.image_render_height = rh.0;

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
                let old_zoom = self.view_state.zoom;
                self.view_state.keyboard_zoom(true);
                if self.auto_fit_window {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
                self.update_transform_and_redraw();
            }
            AppCommand::ZoomOut => {
                let old_zoom = self.view_state.zoom;
                self.view_state.keyboard_zoom(false);
                if self.auto_fit_window {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
                self.update_transform_and_redraw();
            }
            AppCommand::SetZoom(level) => {
                let old_zoom = self.view_state.zoom;
                self.view_state.set_zoom(level);
                if self.auto_fit_window {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
                self.update_transform_and_redraw();
            }
            AppCommand::FitToWindow => {
                let old_zoom = self.view_state.zoom;
                self.view_state.fit_to_window();
                if self.auto_fit_window {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
                self.update_transform_and_redraw();
            }
            AppCommand::ActualSize => {
                let old_zoom = self.view_state.zoom;
                self.view_state.actual_size();
                if self.auto_fit_window {
                    let (cx, cy) = self.window_center_logical();
                    self.auto_fit_after_zoom(old_zoom, cx, cy);
                }
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
            AppCommand::SetIccColorManagement(enabled) => {
                self.icc_color_management = enabled;
                log::info!("ICC color management set to: {enabled}");
                let mut s = settings::Settings::load();
                s.icc_color_management = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.icc_color_management_item.set_checked(enabled);
                    // "Color match display" and "Relative colorimetric" depend on ICC being enabled
                    menu.color_match_item.set_enabled(enabled);
                    menu.relative_colorimetric_item.set_enabled(enabled);
                }
                self.apply_icc_settings();
            }
            AppCommand::SetColorMatchDisplay(enabled) => {
                self.color_match_display = enabled;
                log::info!("Color match display set to: {enabled}");
                let mut s = settings::Settings::load();
                s.color_match_display = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.color_match_item.set_checked(enabled);
                }
                self.apply_icc_settings();
            }
            AppCommand::SetRelativeColorimetric(enabled) => {
                self.use_relative_colorimetric = enabled;
                log::info!(
                    "Rendering intent set to: {}",
                    if enabled {
                        "relative colorimetric"
                    } else {
                        "perceptual"
                    }
                );
                let mut s = settings::Settings::load();
                s.use_relative_colorimetric = enabled;
                s.save();
                if let Some(menu) = &self.app_menu {
                    menu.relative_colorimetric_item.set_checked(enabled);
                }
                self.flush_and_redisplay();
            }
            #[cfg(target_os = "macos")]
            AppCommand::DisplayChanged => {
                self.handle_display_changed();
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
                if !resolved.is_file() {
                    log::warn!("OpenFile: not a file: {}", resolved.display());
                    return;
                }

                // If we were waiting for a file (Finder double-click), initialize the app now
                if self.waiting_for_file {
                    log::info!("File received via Apple Event, initializing viewer");
                    self.waiting_for_file = false;
                    self.wait_start = None;
                    self.file_path = resolved.clone();

                    // Close the onboarding window if it's showing
                    #[cfg(target_os = "macos")]
                    native_ui::close_onboarding_window();

                    // Initialize the full viewer (window, renderer, etc.) via resumed()
                    // by switching control flow — resumed() will be called next
                    self.initialize_viewer(event_loop);
                    return;
                }

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
                        let _ = win.request_inner_size(to_logical_size(
                            Logical(w as f64),
                            Logical(h as f64),
                        ));
                    }
                    if x.is_some() || y.is_some() {
                        let current = win.outer_position().unwrap_or_default();
                        let new_x = x.unwrap_or(current.x);
                        let new_y = y.unwrap_or(current.y);
                        win.set_outer_position(to_logical_pos(
                            Logical(new_x as f64),
                            Logical(new_y as f64),
                        ));
                    }
                    if let Some(renderer) = &mut self.renderer {
                        let (pw, ph) = from_physical_size(win.inner_size());
                        renderer.resize(pw, ph);
                        if let Some((iw, ih)) = self.current_image_size {
                            self.view_state.update_dimensions(
                                iw,
                                ih,
                                renderer.logical_width(),
                                renderer.logical_height(),
                            );
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
                let old_zoom = self.view_state.zoom;
                self.view_state
                    .scroll_zoom(delta, Logical(cursor_x), Logical(cursor_y));
                if self.auto_fit_window {
                    self.auto_fit_after_zoom(
                        old_zoom,
                        Logical(cursor_x as f64),
                        Logical(cursor_y as f64),
                    );
                }
                self.update_transform_and_redraw();
            }
            AppCommand::Refresh => {
                if let Some(path) = self.dir_list.as_ref().map(|d| d.current().to_path_buf()) {
                    self.display_image(&path);
                    self.update_shared_state();
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
            AppCommand::Sync(sender) => {
                self.update_shared_state();
                let _ = sender.send(());
            }
        }
    }
}

impl ApplicationHandler<AppCommand> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Register the event loop proxy globally so native UI delegates can send commands
        qa_server::set_event_loop_proxy(self.event_loop_proxy.clone());

        if self.waiting_for_file {
            // No file yet (Finder double-click or Dock launch). Start the QA server and
            // wait for an Apple Event. The onboarding timer is checked in about_to_wait().
            if self.wait_start.is_none() {
                self.wait_start = Some(Instant::now());
                // Start QA server early so agents can send OpenFile commands
                if self._qa_handle.is_none() {
                    self._qa_handle = qa_server::start(
                        Arc::clone(&self.shared_state),
                        self.event_loop_proxy.clone(),
                    );
                }
                // Use Poll so about_to_wait fires continuously and can check the timer
                event_loop.set_control_flow(ControlFlow::Poll);
            }
            return;
        }

        self.initialize_viewer(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, command: AppCommand) {
        self.execute_command(event_loop, command);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Onboarding timer: if we've been waiting 500ms with no file, show onboarding.
        if self.waiting_for_file {
            if let Some(start) = self.wait_start
                && start.elapsed() >= Duration::from_millis(500)
            {
                log::info!("No Apple Event after 500ms, showing onboarding");
                self.wait_start = None; // Don't fire again
                event_loop.set_control_flow(ControlFlow::Wait);
                #[cfg(target_os = "macos")]
                native_ui::show_onboarding_window_non_modal();
            }
            return;
        }

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
                    let (pw, ph) = from_physical_size(size);
                    renderer.resize(pw, ph);
                    if let Some((iw, ih)) = self.current_image_size {
                        self.view_state.update_dimensions(
                            iw,
                            ih,
                            renderer.logical_width(),
                            renderer.logical_height(),
                        );
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
                    let old_zoom = self.view_state.zoom;
                    let (cx, cy) = self.last_mouse_pos;
                    self.view_state
                        .scroll_zoom(scroll_y, cx.as_f32(), cy.as_f32());
                    if self.auto_fit_window {
                        self.auto_fit_after_zoom(old_zoom, cx, cy);
                    }
                    self.update_transform_and_redraw();
                }
            }

            // Mouse drag for panning (convert to logical pixels)
            WindowEvent::CursorMoved { position, .. } => {
                let sf = self.scale_factor;
                let logical = (Logical(position.x / sf), Logical(position.y / sf));
                let prev = self.last_mouse_pos;
                self.last_mouse_pos = logical;

                if self.drag_start.is_some() {
                    let dx = logical.0 - prev.0;
                    let dy = logical.1 - prev.1;
                    self.view_state.pan(dx.as_f32(), dy.as_f32());
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

            WindowEvent::ScaleFactorChanged {
                scale_factor: new_scale,
                ..
            } => {
                self.scale_factor = new_scale;
                log::debug!("Scale factor changed to {new_scale}");
            }

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
