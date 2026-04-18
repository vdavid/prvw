//! `App` — the core viewer state and event-loop integration.
//!
//! Owns the window, renderer, preloader, image cache, and all user-facing settings.
//! Implements `winit::ApplicationHandler` and dispatches every `AppCommand` through
//! `execute_command` (see `executor.rs`).

mod executor;
mod shared_state;

pub(crate) use shared_state::SharedAppState;

#[cfg(target_os = "macos")]
use crate::color::display_profile;
use crate::commands::{self, AppCommand};
use crate::diagnostics::NavigationRecord;
use crate::navigation::{directory, preloader};
use crate::pixels::{
    Logical, from_logical_pos, from_logical_size, from_physical_size, to_logical_pos,
    to_logical_size,
};
use crate::render::{renderer, text};
#[cfg(target_os = "macos")]
use crate::updater;
use crate::{
    TITLE_BAR_HEIGHT, color, decoding, input, menu, navigation, qa, settings, window, zoom,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

/// Push the user-provided custom DCP directory into the `PRVW_DCP_DIR` env
/// var so `color::dcp::discovery::find_dcp_for_camera` picks it up. When
/// `None` (or an empty string), clear the var so discovery falls back to
/// Adobe Camera Raw's default path and the bundled collection.
///
/// SAFETY: `std::env::set_var` / `remove_var` are unsafe in multi-threaded
/// contexts. We call this from the main thread, before the preloader or
/// QA threads read the var — either at startup or from the command executor,
/// which is single-threaded by construction. Rayon decode tasks read the
/// env var through `discovery::find_dcp_for_camera` on a fresh call each
/// decode, so the worst case is a cached value for one already-in-flight
/// decode, which is harmless.
fn apply_custom_dcp_dir(dir: Option<&str>) {
    let key = crate::color::dcp::discovery::DCP_DIR_ENV_VAR;
    match dir {
        Some(path) if !path.trim().is_empty() => {
            log::info!("DCP: using custom directory {path}");
            // SAFETY: see the function comment.
            unsafe {
                std::env::set_var(key, path);
            }
        }
        _ => {
            // SAFETY: see the function comment.
            unsafe {
                std::env::remove_var(key);
            }
        }
    }
}

/// Application state, created before the event loop starts.
/// The window and renderer are initialized in `resumed()` (required by winit 0.30 on macOS).
pub(crate) struct App {
    // ── Launch ──────────────────────────────────────────────────────
    pub(crate) file_path: PathBuf,
    /// If multiple files were passed on the CLI, use them as the navigation set instead
    /// of scanning the directory.
    pub(crate) explicit_files: Option<Vec<PathBuf>>,
    /// True when launched with no CLI files (Finder double-click or Dock launch).
    pub(crate) waiting_for_file: bool,
    /// When `waiting_for_file`: the time we started waiting. After 500ms with no file,
    /// show the onboarding window.
    pub(crate) wait_start: Option<Instant>,

    // ── Handles ─────────────────────────────────────────────────────
    pub(crate) window: Option<Arc<Window>>,
    pub(crate) renderer: Option<renderer::Renderer>,
    pub(crate) app_menu: Option<menu::AppMenu>,

    // ── Per-feature state ───────────────────────────────────────────
    pub(crate) zoom: zoom::State,
    pub(crate) color: color::State,
    pub(crate) navigation: navigation::State,

    // ── Cross-cutting toggles (owned by App because they don't fit one feature) ──
    /// Whether to reserve space at the top for the title bar.
    pub(crate) title_bar: bool,
    /// Per-stage RAW decode pipeline toggles (Phase 3.7). Production
    /// default is all-true; the Settings → RAW panel flips individual
    /// stages off for transparency and diagnostics.
    pub(crate) raw_flags: crate::decoding::RawPipelineFlags,
    /// Current EDR (extended dynamic range) headroom of the active display
    /// (Phase 5). `1.0` on SDR displays — the decoder produces RGBA8 like
    /// Phase 4. Values above `1.0` switch the RAW decoder into its
    /// `RGBA16F` + filmic-4×-shoulder path so HDR highlights survive to the
    /// renderer. Refreshed on `AppCommand::DisplayChanged` and whenever a
    /// new display is queried.
    pub(crate) edr_headroom: f32,
    /// True when the currently-displayed image's pixel buffer is `Rgba16F`
    /// (Phase 5.1). Combined with `raw_flags.hdr_output` and a non-unity
    /// `edr_headroom`, this determines whether the wgpu surface should be
    /// configured for EDR output. Only RAW decodes produce HDR buffers, so
    /// this flips back to `false` whenever a JPEG / PNG / WebP / etc. loads.
    pub(crate) current_image_is_hdr: bool,

    // ── Runtime input / rendering ───────────────────────────────────
    pub(crate) modifiers: ModifiersState,
    pub(crate) drag_start: Option<(Logical<f64>, Logical<f64>)>,
    pub(crate) last_mouse_pos: (Logical<f64>, Logical<f64>),
    pub(crate) last_click_time: Option<Instant>,
    pub(crate) needs_redraw: bool,
    /// Current display scale factor (Retina = 2.0).
    pub(crate) scale_factor: f64,

    // ── Cross-thread ────────────────────────────────────────────────
    pub(crate) shared_state: Arc<Mutex<SharedAppState>>,
    pub(crate) event_loop_proxy: EventLoopProxy<AppCommand>,
    _qa_handle: Option<std::thread::JoinHandle<()>>,
}

impl App {
    pub(crate) fn new(
        file_path: PathBuf,
        explicit_files: Option<Vec<PathBuf>>,
        waiting_for_file: bool,
        event_loop_proxy: EventLoopProxy<AppCommand>,
        shared_state: Arc<Mutex<SharedAppState>>,
    ) -> Self {
        let initial_settings = settings::Settings::load();
        // Thread the user-provided DCP dir into the decoder via the same env
        // var the DCP discovery module already honors. Done at startup so the
        // very first decode sees it; the SetCustomDcpDir command maintains
        // this in sync on later changes.
        apply_custom_dcp_dir(initial_settings.custom_dcp_dir.as_deref());
        Self {
            file_path,
            explicit_files,
            waiting_for_file,
            wait_start: None,
            window: None,
            renderer: None,
            app_menu: None,
            zoom: zoom::State::from_settings(&initial_settings),
            color: color::State::from_settings(&initial_settings),
            navigation: navigation::State::new(),
            title_bar: initial_settings.title_bar,
            raw_flags: initial_settings.raw,
            edr_headroom: 1.0,
            current_image_is_hdr: false,
            modifiers: ModifiersState::empty(),
            drag_start: None,
            last_mouse_pos: (Logical(0.0), Logical(0.0)),
            last_click_time: None,
            needs_redraw: false,
            scale_factor: 2.0,
            shared_state,
            event_loop_proxy,
            _qa_handle: None,
        }
    }

    /// Compute the content offset based on the title_bar setting and fullscreen state.
    fn content_offset_y(&self) -> Logical<f32> {
        let is_fullscreen = self
            .window
            .as_ref()
            .is_some_and(|w| window::is_fullscreen(w));
        if self.title_bar && !is_fullscreen {
            Logical(TITLE_BAR_HEIGHT)
        } else {
            Logical(0.0)
        }
    }

    /// Apply the current content offset to the view state, resize the window if auto-fit
    /// is on, and recalculate zoom.
    fn apply_content_offset(&mut self) {
        let offset = self.content_offset_y();
        self.zoom.view.set_content_offset_y(offset);
        #[cfg(target_os = "macos")]
        if let Some(win) = &self.window {
            window::set_titlebar_vibrancy_visible(win, offset.0 > 0.0);
        }

        // Resize window to add/remove the title bar area height
        if self.zoom.auto_fit
            && let (Some(win), Some((iw, ih))) = (&self.window, self.navigation.current_image_size)
            && let Some(size) = window::resize_to_fit_image(win, iw, ih, offset)
        {
            let (pw, ph) = from_physical_size(size);
            if let Some(renderer) = &mut self.renderer {
                renderer.resize(pw, ph);
                self.zoom.view.update_dimensions(
                    iw,
                    ih,
                    renderer.logical_width(),
                    renderer.logical_height(),
                );
            }
        }

        self.apply_initial_zoom();
        self.update_transform_and_redraw();
    }

    /// Recalculate the zoom floor based on current image/window/settings state.
    /// Called on image load, window resize, and setting changes. Does NOT change the
    /// current zoom level (only reclamps if it's below the new floor).
    fn update_min_zoom(&mut self) {
        if self.zoom.auto_fit {
            // With auto-fit, the window tracks zoom. The floor is the zoom that would
            // make the window hit the minimum size (200px logical per axis).
            if let Some((iw, ih)) = self.navigation.current_image_size {
                let max_dim = iw.max(ih) as f64;
                self.zoom
                    .view
                    .set_min_zoom((window::MIN_WINDOW_DIM / max_dim) as f32);
            }
            return;
        }

        let fit = self.zoom.view.fit_zoom();
        let is_small = fit > 1.0;
        if is_small && !self.zoom.enlarge {
            self.zoom.view.set_min_zoom(1.0);
        } else {
            self.zoom.view.set_min_zoom(fit);
        }
    }

    /// Compute the target ICC bytes based on current settings.
    /// - ICC off: empty (no transforms)
    /// - ICC on, color match off: sRGB (Level 1)
    /// - ICC on, color match on: display profile (Level 2)
    fn effective_display_icc(&self, window: &Window) -> Vec<u8> {
        if !self.color.icc_enabled {
            return Vec::new(); // No ICC transforms
        }
        #[cfg(target_os = "macos")]
        if self.color.match_display
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
        let Some((iw, ih)) = self.navigation.current_image_size else {
            return;
        };
        let Some(win) = &self.window else {
            return;
        };
        if window::is_fullscreen(win) {
            return;
        }

        let new_zoom = self.zoom.view.zoom;
        let scale = win.scale_factor();
        let offset = self.content_offset_y().0 as f64;

        // Desired window = image * zoom + title bar area offset
        let desired_w = iw as f64 * new_zoom as f64;
        let desired_h = ih as f64 * new_zoom as f64 + offset;

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
            self.zoom.view.pan_x = 0.0;
            self.zoom.view.pan_y = 0.0;
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
            if let Some((iw, ih)) = self.navigation.current_image_size {
                self.zoom.view.update_dimensions(
                    iw,
                    ih,
                    renderer.logical_width(),
                    renderer.logical_height(),
                );
            }
            renderer.update_transform(&self.zoom.view.transform());
        }
    }

    /// Choose the right initial zoom for a newly loaded image.
    /// Sets both the zoom floor and the starting zoom level.
    fn apply_initial_zoom(&mut self) {
        self.update_min_zoom();
        let fit = self.zoom.view.fit_zoom();
        let is_small = fit > 1.0;

        if is_small && !self.zoom.enlarge && !self.zoom.auto_fit {
            self.zoom.view.actual_size(); // show at native pixel size
        } else {
            self.zoom.view.fit_to_window(); // fill the window
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

        // Set up title bar area before any image display
        self.zoom.view.set_content_offset_y(self.content_offset_y());

        // Configure ICC color management based on settings
        self.color.display_icc = self.effective_display_icc(&win);

        // Query the display's EDR headroom. 1.0 on SDR displays (so the
        // RAW decoder stays on the Phase 4 RGBA8 path, bit-identical).
        // XDR and OLED displays return >1.0 which promotes RAWs to the
        // RGBA16F + filmic-4×-shoulder path.
        #[cfg(target_os = "macos")]
        {
            self.edr_headroom = display_profile::current_edr_headroom(&win);
            log::info!("Display EDR headroom: {:.2}", self.edr_headroom);
        }
        let hdr_active = self.raw_flags.hdr_output && self.edr_headroom > 1.0;
        self.navigation.image_cache.set_hdr_mode(hdr_active);
        #[cfg(target_os = "macos")]
        {
            if !self.color.display_icc.is_empty() {
                display_profile::set_layer_colorspace(&win, &self.color.display_icc);
            }
            display_profile::register_screen_change_observer(&win);
            // Allow the title bar area to show vibrancy through the transparent clear.
            display_profile::set_metal_layer_transparent(&win);
            // Push the wgpu Metal layer above the vibrancy views via zPosition so the
            // image renders on top.
            window::push_metal_layer_above_vibrancy(&win);
            // Set initial appearance for windowed mode (image area vibrancy visible).
            window::set_fullscreen_appearance(&win, window::is_fullscreen(&win));
        }

        // Create native menu bar
        self.app_menu = Some(menu::create_menu_bar());

        // Build the navigation list
        self.navigation.dir_list = if let Some(files) = self.explicit_files.take() {
            Some(directory::DirectoryList::from_explicit(files))
        } else {
            directory::DirectoryList::from_file(&self.file_path)
        };

        // Start preloader thread pool
        let mut preloader = preloader::Preloader::start(
            self.color.display_icc.clone(),
            self.color.relative_col,
            self.raw_flags,
            self.edr_headroom,
        );

        // Load and display the initial image
        let initial_path = self.file_path.clone();
        self.display_image(&initial_path);

        if let Some(dir) = &self.navigation.dir_list {
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

        self.navigation.preloader = Some(preloader);
        self.update_shared_state();

        // Start QA server if not already running (it starts early when waiting_for_file)
        if self._qa_handle.is_none() {
            self._qa_handle = qa::start(
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

        match decoding::load_image(
            path,
            &self.color.display_icc,
            self.color.relative_col,
            self.raw_flags,
            self.edr_headroom,
        ) {
            Ok(image) => {
                self.navigation.current_image_size = Some((image.width, image.height));
                self.current_image_is_hdr = image.pixels.is_hdr();
                let offset = self.content_offset_y();

                // Flip the wgpu surface + CAMetalLayer between SDR and EDR
                // based on this image's pixel buffer variant. Must run
                // before `renderer.set_image` so the image-quad pipeline is
                // valid when we draw next.
                self.apply_edr_surface_state();

                let renderer = self.renderer.as_mut().unwrap();

                // Resize window to match image (if enabled and not fullscreen).
                // Use the returned physical size directly — request_inner_size is async,
                // so window.inner_size() would still return the OLD size.
                if self.zoom.auto_fit
                    && let Some(win) = &self.window
                    && let Some(size) =
                        window::resize_to_fit_image(win, image.width, image.height, offset)
                {
                    let (pw, ph) = from_physical_size(size);
                    renderer.resize(pw, ph);
                }

                self.zoom.view.update_dimensions(
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
                    .update_transform(&self.zoom.view.transform());
                self.request_redraw();

                if let Some(dir) = &self.navigation.dir_list {
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

        let offset = self.content_offset_y();
        // First pass: inspect the cached image enough to reconfigure the
        // surface (can't hold an `image_cache` borrow while calling
        // `apply_edr_surface_state`, which needs `&mut self`).
        let cached_meta = self
            .navigation
            .image_cache
            .get(index)
            .map(|img| (img.width, img.height, img.pixels.is_hdr()));

        if let Some((iw, ih, is_hdr)) = cached_meta {
            self.navigation.current_image_size = Some((iw, ih));
            self.current_image_is_hdr = is_hdr;

            // Surface state may need to flip because we navigated to (or
            // from) an HDR decode cached earlier. See `apply_edr_surface_state`.
            self.apply_edr_surface_state();

            // Second pass: grab the image reference for upload.
            let image = self
                .navigation
                .image_cache
                .get(index)
                .expect("image was present a moment ago");

            let renderer = self.renderer.as_mut().unwrap();

            if self.zoom.auto_fit
                && let Some(win) = &self.window
                && let Some(size) = window::resize_to_fit_image(win, iw, ih, offset)
            {
                let (pw, ph) = from_physical_size(size);
                renderer.resize(pw, ph);
            }

            self.zoom.view.update_dimensions(
                iw,
                ih,
                renderer.logical_width(),
                renderer.logical_height(),
            );
            renderer.set_image(image);
            self.apply_initial_zoom();
            self.renderer
                .as_ref()
                .unwrap()
                .update_transform(&self.zoom.view.transform());
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
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.current_index())
            .unwrap_or(0);

        let moved = if let Some(dir) = &mut self.navigation.dir_list {
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
            let dir = self.navigation.dir_list.as_ref().unwrap();
            let indices = dir.preload_range(preloader::preload_count());
            (
                dir.current().to_path_buf(),
                dir.current_index(),
                dir.len(),
                indices,
            )
        };

        let was_cached = self.navigation.image_cache.contains(current_index);
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
        if self.navigation.history.len() >= 10 {
            self.navigation.history.pop_front();
        }
        self.navigation.history.push_back(NavigationRecord {
            from_index,
            to_index: current_index,
            was_cached,
            total_time,
            timestamp: Instant::now(),
        });

        // Cancel stale preload tasks and submit fresh ones for adjacent images
        if let Some(dir) = &self.navigation.dir_list {
            let to_preload: Vec<(usize, PathBuf)> = preload_indices
                .iter()
                .filter(|&&i| !self.navigation.image_cache.contains(i))
                .filter_map(|&i| dir.get(i).map(|p| (i, p.to_path_buf())))
                .collect();

            if let Some(preloader) = &mut self.navigation.preloader {
                preloader.request_preload(to_preload);
            }
        }

        self.update_shared_state();
    }

    fn update_transform_and_redraw(&mut self) {
        log::debug!(
            "View: zoom={:.2}, pan=({:.2}, {:.2})",
            self.zoom.view.zoom,
            self.zoom.view.pan_x,
            self.zoom.view.pan_y
        );
        if let Some(renderer) = &self.renderer {
            renderer.update_transform(&self.zoom.view.transform());
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
        let Some(dir) = &self.navigation.dir_list else {
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

        let zoom_pct = (self.zoom.view.zoom * 100.0).round() as i32;
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
        let Some(preloader) = &mut self.navigation.preloader else {
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
                    self.navigation
                        .image_cache
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

        if color::profiles_match(&self.color.display_icc, &new_icc) {
            return; // No change
        }

        self.color.display_icc = new_icc;

        #[cfg(target_os = "macos")]
        if let Some(win) = &self.window
            && !self.color.display_icc.is_empty()
        {
            display_profile::set_layer_colorspace(win, &self.color.display_icc);
        }

        self.navigation.image_cache.clear();
        if let Some(preloader) = &mut self.navigation.preloader {
            preloader.set_display_icc(self.color.display_icc.clone());
        }
        if let Some(dir) = &self.navigation.dir_list {
            let path = dir.current().to_path_buf();
            self.display_image(&path);
        }
    }

    /// Flush the image cache, update the preloader, and re-decode the current image.
    /// Used when color settings change that don't affect the ICC profile bytes (e.g., rendering intent).
    fn flush_and_redisplay(&mut self) {
        self.navigation.image_cache.clear();
        if let Some(preloader) = &mut self.navigation.preloader {
            preloader.set_use_relative_colorimetric(self.color.relative_col);
        }
        if let Some(dir) = &self.navigation.dir_list {
            let path = dir.current().to_path_buf();
            self.display_image(&path);
        }
    }

    /// Push new RAW pipeline flags into the preloader, flush the cache, and
    /// re-decode. Phase 3.7 Settings → RAW toggles funnel through here.
    /// Phase 5: also retunes the cache's memory budget between SDR (512 MB)
    /// and HDR (1 GB) so the preload count stays constant when the user
    /// flips `hdr_output`.
    pub(crate) fn apply_raw_flag_change(&mut self) {
        let hdr_active = self.raw_flags.hdr_output && self.edr_headroom > 1.0;
        self.navigation.image_cache.set_hdr_mode(hdr_active);
        self.navigation.image_cache.clear();
        if let Some(preloader) = &mut self.navigation.preloader {
            preloader.set_raw_flags(self.raw_flags);
        }
        if let Some(dir) = &self.navigation.dir_list {
            let path = dir.current().to_path_buf();
            // `display_image` updates `current_image_is_hdr` and calls
            // `apply_edr_surface_state`, so the surface picks up the new
            // `hdr_output` flag through the re-decode.
            self.display_image(&path);
        } else {
            // No image yet, but the user toggled hdr_output — make sure
            // the surface matches in case we later load an image from an
            // already-primed cache path.
            self.apply_edr_surface_state();
        }
    }

    /// Single source of truth for "should the wgpu surface run in EDR mode
    /// right now?" All three inputs must hold: the user hasn't opted out,
    /// the display advertises EDR headroom, and the currently-displayed
    /// image is actually an HDR decode. When any flips, call
    /// `apply_edr_surface_state`.
    pub(crate) fn want_edr_surface(&self) -> bool {
        self.raw_flags.hdr_output && self.edr_headroom > 1.0 && self.current_image_is_hdr
    }

    /// Reconfigure the wgpu surface and the `CAMetalLayer` to match
    /// `want_edr_surface()`. No-op when the surface is already in the
    /// right state. Called from image-change, flag-change, and
    /// display-change handlers.
    pub(crate) fn apply_edr_surface_state(&mut self) {
        let want_hdr = self.want_edr_surface();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let changed = renderer.reconfigure_surface_format(want_hdr);
        if !changed {
            return;
        }

        #[cfg(target_os = "macos")]
        if let Some(win) = &self.window {
            display_profile::set_layer_edr_state(win, want_hdr, &self.color.display_icc);
        }

        // After reconfiguring, re-apply the transform so the next frame
        // draws with the new pipelines.
        if let Some(renderer) = &self.renderer {
            renderer.update_transform(&self.zoom.view.transform());
        }
        self.request_redraw();
    }

    /// Sync the custom DCP directory env var and re-decode so the active
    /// DCP lookup picks up the new search root. Called by
    /// `AppCommand::SetCustomDcpDir`.
    pub(crate) fn apply_custom_dcp_dir_change(&mut self, dir: Option<&str>) {
        apply_custom_dcp_dir(dir);
        self.navigation.image_cache.clear();
        if let Some(dir) = &self.navigation.dir_list {
            let path = dir.current().to_path_buf();
            self.display_image(&path);
        }
    }

    /// Re-query the display ICC profile + EDR headroom and re-decode the
    /// current image if either changed. EDR headroom moves with display
    /// switches and with macOS brightness changes, so we refresh it here on
    /// every `DisplayChanged` event.
    #[cfg(target_os = "macos")]
    fn handle_display_changed(&mut self) {
        log::debug!("Display changed, re-evaluating ICC + EDR");
        if let Some(win) = &self.window {
            let new_headroom = display_profile::current_edr_headroom(win);
            if (new_headroom - self.edr_headroom).abs() > 1e-3 {
                log::info!(
                    "EDR headroom changed: {:.2} -> {:.2}",
                    self.edr_headroom,
                    new_headroom
                );
                self.edr_headroom = new_headroom;
                if let Some(preloader) = &mut self.navigation.preloader {
                    preloader.set_edr_headroom(new_headroom);
                }
                let hdr_active = self.raw_flags.hdr_output && new_headroom > 1.0;
                self.navigation.image_cache.set_hdr_mode(hdr_active);
                self.navigation.image_cache.clear();
            }
        }
        self.apply_icc_settings();
        // `apply_icc_settings` re-decodes, which goes through `display_image`
        // and thus `apply_edr_surface_state`. If nothing changed (same
        // display, same ICC), still confirm the surface state matches the
        // latest headroom.
        self.apply_edr_surface_state();
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

            crate::settings::show_settings_window(parent_ptr);
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

            crate::about::show_window(parent_ptr);
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
}

impl ApplicationHandler<AppCommand> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Register the event loop proxy globally so native UI delegates can send commands
        commands::set_event_loop_proxy(self.event_loop_proxy.clone());

        if self.waiting_for_file {
            // No file yet (Finder double-click or Dock launch). Start the QA server and
            // wait for an Apple Event. The onboarding timer is checked in about_to_wait().
            if self.wait_start.is_none() {
                self.wait_start = Some(Instant::now());
                // Start QA server early so agents can send OpenFile commands
                if self._qa_handle.is_none() {
                    self._qa_handle = qa::start(
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
                crate::onboarding::show_window();
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
                if let Some(preloader) = self.navigation.preloader.take() {
                    preloader.shutdown();
                }
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                log::debug!("Window resized to {}x{}", size.width, size.height);
                // Re-apply content offset (may change on fullscreen transitions)
                let offset = self.content_offset_y();
                self.zoom.view.set_content_offset_y(offset);
                #[cfg(target_os = "macos")]
                if let Some(win) = &self.window {
                    window::set_titlebar_vibrancy_visible(win, offset.0 > 0.0);
                    window::set_fullscreen_appearance(win, window::is_fullscreen(win));
                }
                if let Some(renderer) = &mut self.renderer {
                    let (pw, ph) = from_physical_size(size);
                    renderer.resize(pw, ph);
                    if let Some((iw, ih)) = self.navigation.current_image_size {
                        self.zoom.view.update_dimensions(
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
                    renderer.update_transform(&self.zoom.view.transform());
                }
                self.request_redraw();
                self.update_shared_state();
            }

            WindowEvent::RedrawRequested => {
                if self.needs_redraw {
                    log::trace!("Rendering frame");
                    let text_blocks = self.build_text_overlay();
                    let offset = self.content_offset_y();
                    let rendered = self
                        .renderer
                        .as_mut()
                        .is_some_and(|renderer| renderer.render(&text_blocks, offset));
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

            // Scroll: zoom (when scroll_to_zoom is on or Cmd is held) or navigate images
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll_y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / 50.0,
                };
                if scroll_y.abs() > f32::EPSILON {
                    let cmd_held = self.modifiers.super_key();
                    if self.zoom.scroll_to_zoom || cmd_held {
                        // Zoom centered on cursor (Y offset into image area)
                        let old_zoom = self.zoom.view.zoom;
                        let (cx, cy) = self.last_mouse_pos;
                        let offset = Logical(self.content_offset_y().0 as f64);
                        self.zoom
                            .view
                            .scroll_zoom(scroll_y, cx.as_f32(), (cy - offset).as_f32());
                        if self.zoom.auto_fit {
                            self.auto_fit_after_zoom(old_zoom, cx, cy);
                        }
                        self.update_transform_and_redraw();
                    } else {
                        // Navigate: scroll down = next, scroll up = previous
                        let forward = scroll_y < 0.0;
                        self.execute_command(event_loop, AppCommand::Navigate(forward));
                    }
                }
            }

            // Trackpad pinch-to-zoom: cursor-centered
            WindowEvent::PinchGesture { delta, .. } => {
                let delta = delta as f32;
                if delta.abs() > f32::EPSILON {
                    let old_zoom = self.zoom.view.zoom;
                    let (cx, cy) = self.last_mouse_pos;
                    let offset = Logical(self.content_offset_y().0 as f64);
                    self.zoom
                        .view
                        .pinch_zoom(delta, cx.as_f32(), (cy - offset).as_f32());
                    if self.zoom.auto_fit {
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
                    self.zoom.view.pan(dx.as_f32(), dy.as_f32());
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
