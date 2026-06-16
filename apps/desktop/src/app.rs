//! `App` — the core viewer state and event-loop integration.
//!
//! Owns the window, renderer, preloader, image cache, and all user-facing settings.
//! Implements `winit::ApplicationHandler` and dispatches every `AppCommand` through
//! `execute_command` (see `executor.rs`).

mod executor;
#[cfg(target_os = "macos")]
mod previews_hook;
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
    TITLE_BAR_HEIGHT, color, decoding, exif_overlay, histogram, input, menu, navigation, qa,
    settings, slideshow, window, zoom,
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
    /// Keeps the active print operation alive while its window-modal sheet runs (the call
    /// returns immediately; the sheet is async). Replaced on the next print, dropped at exit.
    #[cfg(target_os = "macos")]
    _active_print: Option<objc2::rc::Retained<objc2_app_kit::NSPrintOperation>>,

    // ── Per-feature state ───────────────────────────────────────────
    pub(crate) zoom: zoom::State,
    pub(crate) color: color::State,
    pub(crate) navigation: navigation::State,
    pub(crate) histogram: histogram::State,
    pub(crate) exif_overlay: exif_overlay::State,
    pub(crate) slideshow: slideshow::State,
    /// Browse mode (folder tree + preview grid) vs image viewer. Owns the native
    /// split-view handles on macOS. Starts in `Image`.
    pub(crate) browser: crate::browser::State,
    #[cfg(target_os = "macos")]
    pub(crate) previews: crate::previews::State,

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
    /// Set by a slideshow auto-advance to request that the next image display
    /// crossfades from the current one. Consumed (and cleared) by
    /// `display_from_cache`; cleared on a cache miss so only instant,
    /// already-cached advances crossfade.
    pub(crate) pending_crossfade: bool,
    /// Current display scale factor (Retina = 2.0).
    pub(crate) scale_factor: f64,
    /// Fullscreen state at the last `Resized` event. A fullscreen toggle is async on macOS
    /// (animated), so the reliable signal that the transition settled is the resulting
    /// `Resized`. Comparing against this lets us re-decide the zoom (fit vs actual size) once
    /// per transition without disturbing manual window resizes.
    pub(crate) was_fullscreen: bool,

    // ── Cross-thread ────────────────────────────────────────────────
    pub(crate) shared_state: Arc<Mutex<SharedAppState>>,
    pub(crate) event_loop_proxy: EventLoopProxy<AppCommand>,
    _qa_handle: Option<std::thread::JoinHandle<()>>,

    // ── Preview placeholder tracking ─────────────────────────────
    /// True while the image texture holds a preview placeholder
    /// (uploaded on a cache-miss before the full decode arrives).
    /// Cleared when `display_from_cache` runs with the full image.
    #[cfg(target_os = "macos")]
    pub(crate) placeholder_active: bool,
    /// Monotonic start time for event-timeline timestamps.
    #[cfg(target_os = "macos")]
    pub(crate) app_start: Instant,
    /// Ring buffer of recent preview-lifecycle events. Mirrored to
    /// `SharedAppState` on every `update_shared_state` so MCP clients
    /// can query the timeline after the fact. Capped at 64.
    #[cfg(target_os = "macos")]
    pub(crate) preview_events: std::collections::VecDeque<shared_state::PreviewEvent>,
    /// `Instant::now()` captured at navigation time, keyed by target
    /// index. Used to compute "displayed after Xms" metrics for both
    /// previews and full decodes. Entries are dropped after the full
    /// image is displayed (or on folder change).
    pub(crate) request_times: std::collections::HashMap<usize, Instant>,
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
            #[cfg(target_os = "macos")]
            _active_print: None,
            zoom: zoom::State::from_settings(&initial_settings),
            color: color::State::from_settings(&initial_settings),
            navigation: navigation::State::from_settings(&initial_settings),
            histogram: histogram::State::from_settings(&initial_settings),
            exif_overlay: exif_overlay::State::from_settings(&initial_settings),
            slideshow: slideshow::State::from_settings(&initial_settings),
            browser: crate::browser::State::new(),
            #[cfg(target_os = "macos")]
            previews: crate::previews::State::new(),
            title_bar: initial_settings.title_bar,
            raw_flags: initial_settings.raw,
            edr_headroom: 1.0,
            current_image_is_hdr: false,
            modifiers: ModifiersState::empty(),
            drag_start: None,
            last_mouse_pos: (Logical(0.0), Logical(0.0)),
            last_click_time: None,
            needs_redraw: false,
            pending_crossfade: false,
            scale_factor: 2.0,
            was_fullscreen: false,
            shared_state,
            event_loop_proxy,
            _qa_handle: None,
            #[cfg(target_os = "macos")]
            placeholder_active: false,
            #[cfg(target_os = "macos")]
            app_start: Instant::now(),
            #[cfg(target_os = "macos")]
            preview_events: std::collections::VecDeque::with_capacity(64),
            request_times: std::collections::HashMap::new(),
        }
    }

    /// Compute the content offset based on the title_bar setting and fullscreen state.
    fn content_offset_y(&self) -> Logical<f32> {
        if self.title_bar && !self.is_fullscreen() {
            Logical(TITLE_BAR_HEIGHT)
        } else {
            Logical(0.0)
        }
    }

    /// True when the window is currently fullscreen.
    fn is_fullscreen(&self) -> bool {
        self.window
            .as_ref()
            .is_some_and(|w| window::is_fullscreen(w))
    }

    /// Whether auto-fit actually governs the layout right now. Auto-fit resizes the window to
    /// the image, which is impossible in fullscreen (the window is the whole screen), so it's
    /// inert there — the zoom decision falls back to the fit/enlarge rules instead. Without
    /// this, a small image in fullscreen would be force-fit (enlarged) regardless of the
    /// "Enlarge small images" setting.
    fn effective_auto_fit(&self) -> bool {
        self.zoom.auto_fit && !self.is_fullscreen()
    }

    /// True when the pointer sits over the title bar strip — the top inset reserved when the
    /// title bar is on (and we're not fullscreen). Used to route a double-click there to the
    /// native window zoom instead of the image's fit toggle.
    #[cfg(target_os = "macos")]
    fn pointer_in_title_bar(&self) -> bool {
        let strip = self.content_offset_y().0 as f64;
        strip > 0.0 && self.last_mouse_pos.1.0 < strip
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
        if self.effective_auto_fit() {
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

        if is_small && !self.zoom.enlarge && !self.effective_auto_fit() {
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
            // Keep the traffic lights nudged across relayouts (resize, fullscreen) without a
            // frame of flicker — re-applies the offset synchronously when macOS resets them.
            window::register_traffic_light_keeper(&win);
            // Allow the title bar area to show vibrancy through the transparent clear.
            display_profile::set_metal_layer_transparent(&win);
            // Push the wgpu Metal layer above the vibrancy views via zPosition so the
            // image renders on top.
            window::push_metal_layer_above_vibrancy(&win);
            // Clip the image to the inset rounded rect so the Liquid Glass frame shows.
            window::apply_glass_frame_mask(&win);
            // Set initial appearance for windowed mode (image area vibrancy visible).
            window::set_fullscreen_appearance(&win, window::is_fullscreen(&win));
        }

        // Create native menu bar
        self.app_menu = Some(menu::create_menu_bar());

        // Build the navigation list
        let initial_sort_by = settings::Settings::load().sort_by;
        // The browse grid lists folder images in the same order, so opening a grid item lands on
        // the matching image-mode index.
        self.browser.set_sort_by(initial_sort_by);
        self.navigation.dir_list = if let Some(files) = self.explicit_files.take() {
            Some(directory::DirectoryList::from_explicit(
                files,
                initial_sort_by,
            ))
        } else {
            directory::DirectoryList::from_file(&self.file_path, initial_sort_by)
        };

        // Start preloader thread pool and store it before displaying the
        // initial image, so the async RAW-launch path (see
        // `display_initial_image`) can submit its priority-0 target through
        // `self.navigation.preloader` and `poll_preloader` can drain it.
        self.navigation.preloader = Some(preloader::Preloader::start(
            self.color.display_icc.clone(),
            self.color.relative_col,
            self.raw_flags,
            self.edr_headroom,
            self.event_loop_proxy.clone(),
        ));

        // Seed the preview scheduler with the full folder so every image
        // will get a preview in priority order (indices outside the preload
        // window first). Done BEFORE the initial display so the async RAW path
        // can read source dimensions (via `source_dimensions`'s synchronous
        // fallback over `self.previews.paths`) for the pre-paint auto-fit.
        // The scheduler is paused below, after the display sets
        // `pending_current` on the async path.
        #[cfg(target_os = "macos")]
        if let Some(dir) = &self.navigation.dir_list {
            let paths = dir.files();
            let current = dir.current_index();
            self.previews.set_folder(paths, current);
        }

        // Load and display the initial image. RAW launches take the async
        // quick-preview path (mirrors cache-miss navigation); everything else
        // stays on the synchronous decode (fast enough not to need it).
        let initial_path = self.file_path.clone();
        self.display_initial_image(&initial_path);

        self.warm_initial_neighbors();

        // Pause the preview scheduler while the initial primary decode is
        // running (the async RAW path leaves `pending_current` set). The full
        // decode's arrival in `poll_preloader` resumes it.
        #[cfg(target_os = "macos")]
        if self.navigation.pending_current.is_some() {
            self.previews.pause();
        }
        #[cfg(target_os = "macos")]
        self.pump_preview_requests();

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

    /// Display the initial (launch) image. RAW files take the async
    /// quick-preview path so the window paints the camera's embedded JPEG
    /// preview instantly (then snaps to the full develop when it lands),
    /// instead of blocking the main thread on the ~450 ms RAW develop and
    /// leaving the user on a blank window. Everything else (JPEG, PNG, …)
    /// keeps the synchronous `display_image` decode — those finish in tens of
    /// ms, so an async path would only add a needless "Loading…" flash.
    ///
    /// Mirrors the cache-miss navigation flow (`after_position_change`): set
    /// `pending_current`, size the window from ImageIO dims (no decode), show
    /// the "Loading…" title, and let the preloader's prioritized target ship a
    /// `Preview` then a `Ready` that `poll_preloader` displays.
    /// Warm the preloader's neighbor window around the current image (both sides, since the
    /// direction is unknown at this point). Used by the launch path and by opening an image from
    /// the browse grid. No-op when neighbor preloading is disabled (a benchmark setting).
    fn warm_initial_neighbors(&mut self) {
        let Some(dir) = &self.navigation.dir_list else {
            return;
        };
        if !self.navigation.preload_neighbors {
            log::info!("Preload neighbors disabled — skipping neighbor warm-up");
            return;
        }
        let current_index = dir.current_index();
        let total = dir.len();
        let to_preload: Vec<(usize, PathBuf)> = dir
            .preload_range(
                preloader::preload_count(),
                directory::Direction::Unknown,
                self.navigation.loop_navigation,
            )
            .iter()
            .filter_map(|&i| dir.get(i).map(|p| (i, p.to_path_buf())))
            .collect();
        if !to_preload.is_empty()
            && let Some(preloader) = &mut self.navigation.preloader
        {
            preloader.request_neighbor_preload(to_preload, current_index, total);
        }
    }

    fn display_initial_image(&mut self, path: &Path) {
        let is_raw = path
            .extension()
            .and_then(|e| e.to_str())
            .map(crate::decoding::is_raw_extension)
            .unwrap_or(false);

        let Some((index, total)) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| (d.current_index(), d.len()))
        else {
            // No directory (shouldn't happen — set up just above), but keep the
            // app working with a direct synchronous decode.
            self.display_image(path);
            return;
        };

        if !is_raw {
            // Non-RAW: unchanged synchronous decode, then the final title.
            self.display_image(path);
            if let Some(win) = &self.window {
                window::set_title_keeping_buttons(
                    win,
                    &window::window_title_with_position(path, index, total),
                );
            }
            return;
        }

        // RAW: async quick-preview path.
        log::info!("Initial RAW image — using async quick-preview path");
        self.navigation.pending_current = Some(index);
        self.request_times.insert(index, Instant::now());

        // Size the window from ImageIO dims (metadata-only, no decode) so it's
        // correct before any pixels paint. macOS-only — that's where
        // `source_dimensions` is available. On other platforms the window
        // simply keeps its initial size until the full develop lands.
        #[cfg(target_os = "macos")]
        {
            self.on_primary_decode_started();
            self.apply_preview_auto_fit(index);
        }

        if let Some(win) = &self.window {
            window::set_title_keeping_buttons(win, &window::window_title_loading(index, total));
        }

        if let Some(preloader) = &mut self.navigation.preloader {
            preloader.prioritize_target(index, path.to_path_buf(), total);
        }

        // The window/surface exist, but nothing has painted yet. Request a
        // redraw so the "Loading…" overlay shows immediately, before the first
        // `Preview`/`Ready` arrives.
        self.request_redraw();
    }

    /// Synchronously decode an image and render it. Caches the result under
    /// the dir_list's current index so later navigation hits the cache. Used
    /// by startup, settings re-decode, and `Refresh` — blocking the main
    /// thread is acceptable for these user-initiated paths. Navigation goes
    /// through the preloader instead (see `navigate`).
    fn display_image(&mut self, path: &Path) {
        if self.renderer.is_none() {
            return;
        }

        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let start = Instant::now();
        let result = decoding::load_image(
            path,
            &std::sync::atomic::AtomicBool::new(false),
            &self.color.display_icc,
            self.color.relative_col,
            self.raw_flags,
            self.edr_headroom,
            None, // Synchronous path — never cancelled, so nothing to salvage.
        );

        match result {
            Ok(image) => {
                let duration = start.elapsed();
                // We cleared whatever we were waiting on; this path replaces it.
                self.navigation.pending_current = None;

                if let Some(dir) = &self.navigation.dir_list {
                    let index = dir.current_index();
                    let total = dir.len();
                    let cache_path = path.to_path_buf();
                    let evicted = self.navigation.image_cache.insert(
                        cache_path,
                        image,
                        duration,
                        filename.clone(),
                    );
                    self.log_evictions(evicted, "LRU");
                    if self.display_from_cache(index) {
                        log::info!("Displayed {filename} ({}/{})", index + 1, total);
                    }
                } else {
                    // Shouldn't happen in practice — every call site sets up
                    // dir_list first — but fall back to a direct render so
                    // the app keeps working.
                    self.display_decoded_direct(&image);
                    log::info!("Displayed {filename}");
                }
            }
            Err(msg) => {
                log::error!("{msg}");
                if let Some(win) = &self.window {
                    window::set_title_keeping_buttons(win, &format!("Prvw - {msg}"));
                }
            }
        }
    }

    /// Emit a `Cache evicted ...` debug line for each entry the cache just
    /// dropped. `reason` is a short tag (`LRU`, `out of window`, ...) that
    /// goes into the message so the logs are easy to scan.
    fn log_evictions(&self, evicted: Vec<preloader::EvictedEntry>, reason: &str) {
        if evicted.is_empty() {
            return;
        }
        for e in evicted {
            log::debug!(
                "Evicted {} from memory - {} freed ({reason})",
                e.file_name,
                navigation::format_bytes(e.memory_cost),
            );
        }
    }

    /// Render an image from the cache at `index`. Returns true when the image
    /// was present and rendered; false on cache miss. Touches LRU order.
    fn display_from_cache(&mut self, index: usize) -> bool {
        if self.renderer.is_none() {
            return false;
        }
        let Some(path) = self
            .navigation
            .dir_list
            .as_ref()
            .and_then(|d| d.get(index))
            .map(|p| p.to_path_buf())
        else {
            return false;
        };
        // Decide on a crossfade before `prepare_display` mutates the view: we
        // need the outgoing image's transform and the pre-resize surface size.
        let want_crossfade = self.pending_crossfade && self.slideshow.crossfade_enabled;
        self.pending_crossfade = false;
        let prev_transform = self.zoom.view.transform();
        let size_before = self
            .renderer
            .as_ref()
            .map(|r| (r.logical_width().0, r.logical_height().0));
        let had_image = self.renderer.as_ref().is_some_and(|r| r.has_image());

        // First pass: inspect the cached image enough to reconfigure the
        // surface (can't hold an `image_cache` borrow while calling
        // `prepare_display`, which needs `&mut self`).
        let Some((iw, ih, is_hdr)) = self
            .navigation
            .image_cache
            .get(&path)
            .map(|img| (img.width, img.height, img.pixels.is_hdr()))
        else {
            return false;
        };
        self.prepare_display(iw, ih, is_hdr);

        // Only crossfade when the surface size is unchanged: a window resize
        // (auto-fit on a differently-sized image) would render the outgoing
        // image with a stale transform, so we cut instead. Same-sized
        // consecutive images — the common case in a folder of camera shots —
        // crossfade cleanly.
        let size_after = self
            .renderer
            .as_ref()
            .map(|r| (r.logical_width().0, r.logical_height().0));
        let do_crossfade = want_crossfade && had_image && size_before == size_after;
        // This display supersedes any prior in-flight fade (e.g. a manual nav
        // landing mid-crossfade). Drop it before maybe starting a new one so
        // the renderer never blends against a stale outgoing texture.
        self.slideshow.crossfade = None;
        if let Some(r) = self.renderer.as_mut() {
            if do_crossfade {
                r.begin_crossfade(&prev_transform);
            } else {
                r.end_crossfade();
            }
        }

        // Second pass: grab the image reference for upload + histogram.
        let image = self
            .navigation
            .image_cache
            .get(&path)
            .expect("image was present a moment ago");
        // Histogram compute is gated on visibility. Off-by-default users
        // pay zero cost; toggling it on later computes lazily from the
        // cached `DecodedImage` (see `ToggleHistogram` arm in `executor.rs`).
        let new_histogram = if self.histogram.visible {
            Some(histogram::compute::compute(&image.pixels))
        } else {
            None
        };
        if let Some(renderer) = &mut self.renderer {
            renderer.set_image(image);
        }
        self.histogram.data = new_histogram;
        self.histogram.hover_bin = None;
        self.finalize_display();
        if do_crossfade {
            // Start the fade with the incoming image fully transparent; the
            // per-frame ramp lives in `drive_crossfade`.
            self.slideshow.crossfade = Some(Instant::now());
            let base = self.zoom.view.transform();
            if let Some(r) = &self.renderer {
                r.set_crossfade(&base, 0.0);
            }
            self.request_redraw();
        }
        // The slideshow dwell starts when an image is actually on screen, not
        // when it was requested. So a slow decode doesn't eat into (or skip)
        // the next slide's time — each image gets its full interval once shown.
        if self.slideshow.running {
            self.slideshow.next_advance = Some(Instant::now() + self.slideshow.interval());
        }
        true
    }

    /// Shared first half of the display pipeline: update dimensions, flip
    /// EDR surface if needed, auto-fit the window, sync the zoom view.
    /// Caller uploads the texture between this and `finalize_display`.
    ///
    /// Used by both the cached-image path (`display_from_cache`) and the
    /// preview placeholder path (`display_preview_placeholder`) so a
    /// preview is fitted with exactly the same math as the final image.
    /// Without this, a preview shown as a placeholder ended up at whatever
    /// zoom the previous image left behind (often 1:1, looking like a
    /// crop of the wrong picture).
    fn prepare_display(&mut self, source_width: u32, source_height: u32, is_hdr: bool) {
        let offset = self.content_offset_y();
        self.navigation.current_image_size = Some((source_width, source_height));
        self.current_image_is_hdr = is_hdr;
        self.apply_edr_surface_state();

        if self.zoom.auto_fit
            && let Some(win) = &self.window
            && let Some(size) =
                window::resize_to_fit_image(win, source_width, source_height, offset)
        {
            let (pw, ph) = from_physical_size(size);
            if let Some(renderer) = &mut self.renderer {
                renderer.resize(pw, ph);
            }
        }
        if let Some(renderer) = &self.renderer {
            let lw = renderer.logical_width();
            let lh = renderer.logical_height();
            self.zoom
                .view
                .update_dimensions(source_width, source_height, lw, lh);
        }
    }

    /// Shared second half of the display pipeline: choose initial zoom
    /// from settings (`apply_initial_zoom`), push the transform to the
    /// GPU, request a redraw. Call after `renderer.set_image`.
    fn finalize_display(&mut self) {
        self.apply_initial_zoom();
        if let Some(renderer) = &self.renderer {
            renderer.update_transform(&self.zoom.view.transform());
        }
        self.request_redraw();
    }

    /// Fallback render for the (rare) case where `display_image` succeeds
    /// without a dir_list — nothing to cache into, so upload directly.
    fn display_decoded_direct(&mut self, image: &decoding::DecodedImage) {
        self.navigation.current_image_size = Some((image.width, image.height));
        self.current_image_is_hdr = image.pixels.is_hdr();
        let offset = self.content_offset_y();
        self.apply_edr_surface_state();
        let renderer = self.renderer.as_mut().unwrap();
        if self.zoom.auto_fit
            && let Some(win) = &self.window
            && let Some(size) = window::resize_to_fit_image(win, image.width, image.height, offset)
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
        renderer.set_image(image);
        // Lazy compute: skip histogram work when the panel is hidden.
        self.histogram.data = if self.histogram.visible {
            Some(histogram::compute::compute(&image.pixels))
        } else {
            None
        };
        self.histogram.hover_bin = None;
        self.apply_initial_zoom();
        self.renderer
            .as_ref()
            .unwrap()
            .update_transform(&self.zoom.view.transform());
        self.request_redraw();
    }

    /// Queue a single nav step for the debounced path. A burst of these
    /// within `navigation::NAV_DEBOUNCE` collapses into one jump.
    ///
    /// Also previews the prospective target in the window title
    /// **immediately**, so a press always lights up the title bar even
    /// before the debounce window flushes. Without this, mashing left
    /// 4 times shows nothing for 30 ms (then jumps to the final target);
    /// with it, the title walks `10 / 38 → 9 / 38 → 8 / 38 …` in real
    /// time. The actual decode + display still debounces — only the
    /// preview is eager.
    pub(crate) fn queue_nav_step(&mut self, event_loop: &ActiveEventLoop, step: i32) {
        self.navigation.pending_nav_delta = self.navigation.pending_nav_delta.saturating_add(step);
        let deadline = Instant::now() + navigation::NAV_DEBOUNCE;
        self.navigation.nav_deadline = Some(deadline);
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));

        if let Some(dir) = &self.navigation.dir_list
            && let Some(win) = &self.window
        {
            let total = dir.len() as i64;
            let target = (dir.current_index() as i64 + self.navigation.pending_nav_delta as i64)
                .clamp(0, total.saturating_sub(1)) as usize;
            if let Some(path) = dir.get(target) {
                window::set_title_keeping_buttons(
                    win,
                    &window::window_title_with_position(path, target, dir.len()),
                );
            }
        }
    }

    /// Apply any pending debounced delta immediately. Called before
    /// immediate-nav commands (QA / MCP / HTTP) so tests don't race the
    /// deadline, and from `about_to_wait` when the deadline fires.
    pub(crate) fn flush_pending_nav(&mut self) {
        let delta = self.navigation.pending_nav_delta;
        self.navigation.pending_nav_delta = 0;
        self.navigation.nav_deadline = None;
        if delta != 0 {
            self.navigate_by(delta);
        }
    }

    fn navigate_by(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let from_index = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.current_index())
            .unwrap_or(0);

        let loop_on = self.navigation.loop_navigation;
        let moved_delta = if let Some(dir) = &mut self.navigation.dir_list {
            dir.go_by(delta, loop_on)
        } else {
            0
        };

        if moved_delta == 0 {
            return;
        }

        // With loop on, wrap-around at the last->first edge produces a
        // negative net delta even though the user moved "forward". Use the
        // requested delta's sign for the direction hint instead.
        let forward = if loop_on { delta > 0 } else { moved_delta > 0 };
        self.after_position_change(from_index, Some(forward));
    }

    /// Absolute jump to the first image. No-op when already at index 0,
    /// the directory is empty, or no directory is loaded. Mirrors
    /// `navigate_by`'s post-move flow exactly.
    pub(crate) fn navigate_to_first(&mut self) {
        let from_index = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.current_index())
            .unwrap_or(0);
        let moved = self
            .navigation
            .dir_list
            .as_mut()
            .map(|d| d.go_to_first())
            .unwrap_or(0);
        if moved == 0 {
            return;
        }
        // Absolute jumps aren't directional — preload both sides equally.
        self.after_position_change(from_index, None);
    }

    /// Absolute jump to the last image. No-op when already at the last
    /// index, the directory is empty, or no directory is loaded.
    pub(crate) fn navigate_to_last(&mut self) {
        let from_index = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.current_index())
            .unwrap_or(0);
        let moved = self
            .navigation
            .dir_list
            .as_mut()
            .map(|d| d.go_to_last())
            .unwrap_or(0);
        if moved == 0 {
            return;
        }
        self.after_position_change(from_index, None);
    }

    // ── Slideshow ────────────────────────────────────────────────────

    /// Start or stop the slideshow (Slideshow → Start/Stop, ⌘S).
    pub(crate) fn toggle_slideshow(&mut self) {
        if self.slideshow.running {
            self.stop_slideshow();
        } else {
            self.start_slideshow();
        }
    }

    pub(crate) fn start_slideshow(&mut self) {
        self.slideshow.running = true;
        self.slideshow.next_advance = Some(Instant::now() + self.slideshow.interval());
        log::info!(
            "Slideshow started ({}s/image, crossfade={}, loop={})",
            self.slideshow.seconds,
            self.slideshow.crossfade_enabled,
            self.slideshow.loop_enabled
        );
        self.set_slideshow_menu_label();
        self.update_shared_state();
    }

    pub(crate) fn stop_slideshow(&mut self) {
        if !self.slideshow.running {
            return;
        }
        self.slideshow.running = false;
        self.slideshow.next_advance = None;
        self.pending_crossfade = false;
        // Let any in-flight crossfade finish naturally; just stop scheduling.
        log::info!("Slideshow stopped");
        self.set_slideshow_menu_label();
        self.update_shared_state();
    }

    /// Update the Start/Stop menu item's label to match the running state.
    fn set_slideshow_menu_label(&self) {
        if let Some(menu) = &self.app_menu {
            let label = if self.slideshow.running {
                "Stop slideshow"
            } else {
                "Start slideshow"
            };
            menu.slideshow_toggle_item.set_text(label);
        }
    }

    // ── Browse mode ──────────────────────────────────────────────────

    /// Switch the main window between the image viewer and the native browse screen.
    /// No-op when already in `target`. Entering browse shows the split view and hides the
    /// Metal layer (the GPU goes idle — no redraws requested); entering image reverses it
    /// and requests a redraw. Also flips the Navigate menu item's label.
    /// Open the grid's selected image in image mode (double-click on a grid item, or Enter while
    /// the grid pane is focused). Sets up `navigation` for the selected folder's images at the
    /// chosen index, displays that image, and switches to image mode so arrow-key navigation works
    /// afterward. No-op off macOS or when the grid has no selection (empty folder).
    pub(crate) fn open_selected_grid_image(&mut self) {
        #[cfg(target_os = "macos")]
        {
            // Enter on the tree (or with no grid selection) just returns to image mode showing the
            // current image — only a focused grid with a selection opens a specific image.
            let grid_focused =
                matches!(self.browser.focused_pane(), Some(crate::browser::PaneSide::Grid));
            let target = if grid_focused {
                self.browser.grid_open_target()
            } else {
                None
            };
            let Some((path, images, index)) = target else {
                self.set_view_mode(crate::browser::ViewMode::Image);
                return;
            };
            log::info!(
                "Browse open: {} (index {index} of {})",
                path.display(),
                images.len()
            );

            // Build the navigation list from the grid's image list (same `SortBy` as the grid, so
            // the order matches) and position it at the chosen index. `from_explicit` re-sorts with
            // the same comparator, so `go_by(index)` lands on `path`.
            let sort_by = settings::Settings::load().sort_by;
            let mut dir_list = directory::DirectoryList::from_explicit(images, sort_by);
            if index > 0 {
                dir_list.go_by(index as i32, false);
            }
            self.navigation.dir_list = Some(dir_list);

            // Leave browse for image mode first so the Metal layer is visible before we paint.
            self.set_view_mode(crate::browser::ViewMode::Image);

            // Seed the preview scheduler with the new folder (placeholder support + dim prefetch).
            if let Some(dir) = &self.navigation.dir_list {
                let paths = dir.files();
                let current = dir.current_index();
                self.previews.set_folder(paths, current);
            }

            // Display the chosen image (synchronous decode — a user-initiated open, like launch).
            self.display_image(&path);
            if let Some((dir_index, total)) = self
                .navigation
                .dir_list
                .as_ref()
                .map(|d| (d.current_index(), d.len()))
                && let Some(win) = &self.window
            {
                window::set_title_keeping_buttons(
                    win,
                    &window::window_title_with_position(&path, dir_index, total),
                );
            }

            // Warm neighbors so arrow-key nav is instant, mirroring the launch path.
            self.warm_initial_neighbors();
            self.pump_preview_requests();
            self.update_shared_state();
        }
    }

    pub(crate) fn set_view_mode(&mut self, target: crate::browser::ViewMode) {
        if self.browser.mode() == target {
            return;
        }
        self.browser.toggle_mode();

        #[cfg(target_os = "macos")]
        if let Some(win) = self.window.clone() {
            match target {
                crate::browser::ViewMode::Browse => self.browser.enter_browse(&win),
                crate::browser::ViewMode::Image => self.browser.enter_image(&win),
            }
        }

        self.set_browse_menu_label();

        // Image mode needs a frame; browse mode goes idle (render-on-demand).
        if matches!(target, crate::browser::ViewMode::Image) {
            self.request_redraw();
        }
        self.update_shared_state();
    }

    /// Update the Navigate menu item's label to match the current mode: "Image browser" while
    /// viewing an image (the action takes you there), "Image view" while browsing.
    fn set_browse_menu_label(&self) {
        if let Some(menu) = &self.app_menu {
            let label = if self.browser.is_browse() {
                "Image view"
            } else {
                "Image browser"
            };
            menu.browse_toggle_item.set_text(label);
        }
    }

    /// If the slideshow is running, push the next auto-advance out by one full
    /// interval. Called when the user navigates manually so the slide they
    /// just chose gets its full dwell time.
    pub(crate) fn slideshow_bump_timer(&mut self) {
        if self.slideshow.running {
            self.slideshow.next_advance = Some(Instant::now() + self.slideshow.interval());
        }
    }

    /// The index the next auto-advance would land on, honoring looping.
    /// `None` at the last image with looping off (the advance will stop the
    /// show instead).
    fn slideshow_next_index(&self) -> Option<usize> {
        let dir = self.navigation.dir_list.as_ref()?;
        let total = dir.len();
        if total == 0 {
            return None;
        }
        let cur = dir.current_index();
        if cur + 1 < total {
            Some(cur + 1)
        } else if self.slideshow.loop_enabled {
            Some(0)
        } else {
            None
        }
    }

    /// Whether it's safe to auto-advance right now: the current image is fully
    /// displayed (not a "Loading…" placeholder) and the next image is already
    /// decoded. This holds the slideshow on the current image until the switch
    /// can be instant and clean, even if a large image takes longer than the
    /// per-image interval to decode. When neighbor preloading is off (a
    /// benchmark setting), the next-cached requirement is skipped so the
    /// slideshow can't stall.
    fn slideshow_ready_to_advance(&self) -> bool {
        if self.navigation.pending_current.is_some() {
            return false;
        }
        if !self.navigation.preload_neighbors {
            return true;
        }
        match self.slideshow_next_index() {
            None => true, // at the last image, looping off — the advance stops the show
            Some(idx) => self
                .navigation
                .dir_list
                .as_ref()
                .and_then(|dir| dir.get(idx))
                .is_none_or(|p| self.navigation.image_cache.contains(p)),
        }
    }

    /// Advance to the next slide. At the last image, wrap to the first when
    /// looping is on, otherwise stop. Reschedules the next advance.
    fn slideshow_advance(&mut self) {
        let Some((index, total)) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| (d.current_index(), d.len()))
        else {
            self.stop_slideshow();
            return;
        };

        if total <= 1 {
            // Nothing to advance to; keep running and check again next tick.
            self.slideshow.next_advance = Some(Instant::now() + self.slideshow.interval());
            return;
        }

        let at_last = index + 1 >= total;
        if at_last && !self.slideshow.loop_enabled {
            self.stop_slideshow();
            return;
        }

        self.flush_pending_nav();
        self.pending_crossfade = self.slideshow.crossfade_enabled;
        if at_last {
            self.navigate_to_first();
        } else {
            self.navigate_by(1);
        }
        self.slideshow.next_advance = Some(Instant::now() + self.slideshow.interval());
    }

    /// Step the time-per-image one notch (`[` / `]` and the Slideshow menu).
    /// Persists the new value and, while running, restarts the dwell timer so
    /// the change takes effect immediately.
    pub(crate) fn adjust_slideshow_speed(&mut self, faster: bool) {
        let new_seconds = slideshow::stepped_seconds(self.slideshow.seconds, faster);
        if new_seconds == self.slideshow.seconds {
            return;
        }
        self.slideshow.seconds = new_seconds;
        log::info!("Slideshow speed: {new_seconds}s/image");
        let mut s = settings::Settings::load();
        s.slideshow_seconds = new_seconds;
        s.save();
        self.slideshow_bump_timer();
        self.update_shared_state();
    }

    /// Advance the in-flight crossfade by one frame: recompute the fade factor
    /// from elapsed time, push it to the renderer, and request a redraw. When
    /// the fade completes, drop the outgoing texture.
    fn drive_crossfade(&mut self) {
        let Some(start) = self.slideshow.crossfade else {
            return;
        };
        let progress =
            (start.elapsed().as_secs_f32() / slideshow::CROSSFADE_DURATION.as_secs_f32()).min(1.0);
        let base = self.zoom.view.transform();
        if let Some(r) = &self.renderer {
            r.set_crossfade(&base, progress);
        }
        self.needs_redraw = true;
        if let Some(win) = &self.window {
            win.request_redraw();
        }
        if progress >= 1.0 {
            self.slideshow.crossfade = None;
            if let Some(r) = &mut self.renderer {
                r.end_crossfade();
            }
        }
    }

    /// Pick the earliest pending wakeup across the nav debounce, the slideshow
    /// timer, and the crossfade animation, and set winit's control flow
    /// accordingly. Falls back to `Wait` when nothing is pending.
    fn schedule_wakeup(&self, event_loop: &ActiveEventLoop) {
        let mut candidates: Vec<Instant> = Vec::new();
        if let Some(d) = self.navigation.nav_deadline {
            candidates.push(d);
        }
        if self.slideshow.running
            && let Some(t) = self.slideshow.next_advance
        {
            if t > Instant::now() {
                // Normal case: wake at the dwell deadline.
                candidates.push(t);
            } else {
                // Deadline passed but we're holding for readiness (current
                // still decoding, or next image not cached). A preloader
                // completion event wakes us earlier to re-check; the grace cap
                // is the backstop so a corrupt/never-decoding next image can't
                // stall the show. Either way we avoid busy-spinning on
                // `WaitUntil(past)`.
                candidates.push(t + slideshow::MAX_HOLD);
            }
        }
        if self.slideshow.crossfade.is_some() {
            // ~60 fps while the fade runs.
            candidates.push(Instant::now() + Duration::from_millis(16));
        }
        // Browse-mode loading overlay: if a tree scan is in flight, wake at its 1s deadline so
        // `about_to_wait` can reveal the "Loading…" overlay (fast scans finish before then and
        // never schedule a wakeup that matters). macOS-only — the tree is AppKit.
        #[cfg(target_os = "macos")]
        if let Some(earliest) = self.browser.earliest_in_flight_scan() {
            candidates.push(earliest + crate::browser::tree_model::LOADING_OVERLAY_DELAY);
        }
        match candidates.into_iter().min() {
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    /// Shared post-move flow used by `navigate_by`, `navigate_to_first`, and
    /// `navigate_to_last`. Assumes the directory cursor has just moved.
    /// `forward_hint` drives the preload priority direction. `None` means
    /// "non-directional jump" (Home / End): preload both sides equally.
    fn after_position_change(&mut self, from_index: usize, forward_hint: Option<bool>) {
        let nav_start = Instant::now();
        let direction = match forward_hint {
            Some(true) => "next",
            Some(false) => "prev",
            None => "jump",
        };

        // Record the travel direction so neighbor priority follows the user.
        self.navigation.last_direction = match forward_hint {
            Some(true) => directory::Direction::Forward,
            Some(false) => directory::Direction::Backward,
            None => directory::Direction::Unknown,
        };

        // Extract what we need from dir_list before mutable borrow
        let (current_path, current_index, total, preload_indices) = {
            let dir = self.navigation.dir_list.as_ref().unwrap();
            let indices = dir.preload_range(
                preloader::preload_count(),
                self.navigation.last_direction,
                self.navigation.loop_navigation,
            );
            (
                dir.current().to_path_buf(),
                dir.current_index(),
                dir.len(),
                indices,
            )
        };

        let was_cached = self.navigation.image_cache.contains(&current_path);
        let cached_str = if was_cached { "yes" } else { "no" };
        log::debug!("Navigate {direction}: {from_index} -> {current_index} (cached: {cached_str})");

        // Mark the request time for this target so we can log "displayed
        // after Xms" when the image or placeholder actually appears.
        let request_start = Instant::now();
        self.request_times.insert(current_index, request_start);
        log::info!(
            "Requested image #{current_index} ({}/{})",
            current_index + 1,
            total
        );

        if was_cached {
            // Cached — render immediately and clear any pending target.
            self.navigation.pending_current = None;
            if let Some(win) = &self.window {
                window::set_title_keeping_buttons(
                    win,
                    &window::window_title_with_position(&current_path, current_index, total),
                );
            }
            self.display_from_cache(current_index);
            let elapsed = request_start.elapsed().as_millis();
            log::info!("Image #{current_index} displayed after {elapsed}ms (cached)");
            self.request_times.remove(&current_index);
            #[cfg(target_os = "macos")]
            {
                self.on_primary_decode_settled();
                self.on_preview_current_changed(current_index);
            }
        } else {
            // Cache miss — show "Loading…" title and mark pending.
            // The render happens in `poll_preloader` when `Ready` arrives.
            // No crossfade for a miss: the preview placeholder would become
            // the "outgoing" frame, and an advance that has to wait on a
            // decode isn't a smooth transition anyway.
            self.pending_crossfade = false;
            self.navigation.pending_current = Some(current_index);
            if let Some(win) = &self.window {
                window::set_title_keeping_buttons(
                    win,
                    &window::window_title_loading(current_index, total),
                );
            }
            #[cfg(target_os = "macos")]
            {
                let preview_cached = self.previews.get(current_index).is_some();
                self.record_preview_event(
                    "nav-cache-miss",
                    format!("from={from_index} to={current_index} preview_cached={preview_cached}"),
                );
                self.on_primary_decode_started();
                self.on_preview_current_changed(current_index);
                // Try to upload the preview placeholder — that path now
                // also resizes the window and applies the initial zoom
                // via `prepare_display`. If no preview is cached yet, fall
                // back to a metadata-only auto-fit so the window still
                // reaches the right size before pixels arrive.
                if !self.display_preview_placeholder(current_index) {
                    self.apply_preview_auto_fit(current_index);
                }
                self.needs_redraw = true;
            }
        }

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

        // Submit preload tasks. Two cases:
        //
        // - **Cache miss**: queue ONLY the priority-0 task (current target).
        //   Defer neighbor submission to `submit_neighbor_preload`, which
        //   the `PreloadResponse::Ready` arm of `poll_preloader` calls
        //   after the primary arrives. This keeps the FIFO channel small
        //   during rapid navigation — without this, every nav adds the
        //   target plus 4 neighbors, and a 5-nav burst piles up ~20 tasks
        //   ahead of the latest target. Neighbors are still pre-decoded
        //   for the image the user actually lands on.
        //
        // - **Cache hit**: no priority-0 needed (already in cache).
        //   Submit neighbors immediately to keep the cache warm.
        if !was_cached {
            if let Some(preloader) = &mut self.navigation.preloader {
                preloader.prioritize_target(current_index, current_path, total);
            }
        } else if self.navigation.preload_neighbors {
            self.submit_neighbor_preload(current_index, total, &preload_indices);
        }

        // Drop cache entries outside the hot window. Keeps RAM bounded even
        // when the LRU budget isn't hit (for example, lots of small JPEGs).
        // The window is current ± PRELOAD_AHEAD on both sides regardless of
        // travel direction (the user can reverse at any time). With loop
        // navigation on, the window wraps so the wrap-side neighbours stay
        // resident.
        let keep_indices = navigation::wrap::active_preload_indices(
            current_index,
            total,
            preloader::preload_count(),
            self.navigation.loop_navigation,
        );
        let keep_paths: Vec<PathBuf> = self
            .navigation
            .dir_list
            .as_ref()
            .map(|dir| {
                keep_indices
                    .iter()
                    .filter_map(|&i| dir.get(i).map(|p| p.to_path_buf()))
                    .collect()
            })
            .unwrap_or_default();
        let evicted = self.navigation.image_cache.retain_only(&keep_paths);
        self.log_evictions(evicted, "out of window");

        self.update_shared_state();
    }

    /// Recompute the active preload window around the current image after a
    /// structural re-shuffle (loop-navigation toggle, sort change). Drops
    /// cache entries that are no longer in the hot window, then queues
    /// preloads for newly-in-window indices that aren't already cached.
    /// Fire-and-forget; the user doesn't wait on these decodes.
    pub(crate) fn refresh_preload_window(&mut self) {
        let Some(dir) = &self.navigation.dir_list else {
            return;
        };
        let total = dir.len();
        if total == 0 {
            return;
        }
        let current_index = dir.current_index();
        let active = navigation::wrap::active_preload_indices(
            current_index,
            total,
            preloader::preload_count(),
            self.navigation.loop_navigation,
        );
        let active_paths: Vec<PathBuf> = active
            .iter()
            .filter_map(|&i| dir.get(i).map(|p| p.to_path_buf()))
            .collect();
        let evicted = self.navigation.image_cache.retain_only(&active_paths);
        self.log_evictions(evicted, "loop toggle");

        if !self.navigation.preload_neighbors {
            return;
        }
        let preload_indices: Vec<usize> = active
            .iter()
            .copied()
            .filter(|&i| i != current_index)
            .collect();
        if preload_indices.is_empty() {
            return;
        }
        self.submit_neighbor_preload(current_index, total, &preload_indices);
    }

    /// Paths currently inside the hot preload window (current ± `PRELOAD_AHEAD`,
    /// wrap-aware). Used by `poll_preloader` to decide whether a salvaged decode
    /// is still worth keeping. Empty when no directory is loaded. Mirrors the
    /// keep-set logic in `after_position_change` / `refresh_preload_window`.
    fn current_window_keep_paths(&self) -> Vec<PathBuf> {
        let Some(dir) = &self.navigation.dir_list else {
            return Vec::new();
        };
        let total = dir.len();
        if total == 0 {
            return Vec::new();
        }
        navigation::wrap::active_preload_indices(
            dir.current_index(),
            total,
            preloader::preload_count(),
            self.navigation.loop_navigation,
        )
        .iter()
        .filter_map(|&i| dir.get(i).map(|p| p.to_path_buf()))
        .collect()
    }

    /// Show a RAW's embedded-JPEG preview as a soft placeholder while the full
    /// develop runs. Like `display_preview_placeholder`, but uses the
    /// passed-in preview (higher-res than the QL preview, and available without
    /// quicklookd, so it covers the first-visit / no-preview case). Source dims
    /// drive window/zoom — already set by `apply_preview_auto_fit` on the
    /// cache-miss, so the resize is a no-op. The full decode replaces this when
    /// `Ready` arrives; the "Loading…" overlay stays up meanwhile (it's gated
    /// on `pending_current`), signalling the soft image isn't final.
    ///
    /// macOS-only: the soft-placeholder path reads QuickLook-backed preview
    /// state (`previews` source dims, `placeholder_active`), both gated to
    /// macOS. RAW preview decode itself is cross-platform, but its display isn't.
    #[cfg(target_os = "macos")]
    fn display_raw_preview_placeholder(&mut self, index: usize, image: decoding::DecodedImage) {
        if self.renderer.is_none() {
            return;
        }
        let dims = self
            .previews
            .source_dimensions(index)
            .map(|d| (d.width, d.height));
        let (sw, sh) = dims
            .or(self.navigation.current_image_size)
            .unwrap_or((image.width, image.height));
        self.prepare_display(sw, sh, false);
        if let Some(renderer) = &mut self.renderer {
            renderer.set_image(&image);
        }
        // Drop the previous image's histogram; the full decode recomputes it.
        self.histogram.data = None;
        self.histogram.hover_bin = None;
        self.finalize_display();
        self.placeholder_active = true;
    }

    /// Queue background preload tasks for the neighbors of `index`.
    /// Skips indices already in the image cache. Called both from a
    /// cache-hit nav (immediately) and from `poll_preloader` after a
    /// cache-miss primary arrives — in the latter case the target's
    /// neighbors weren't queued at nav time so the FIFO channel could
    /// stay small during rapid navigation.
    fn submit_neighbor_preload(
        &mut self,
        current_index: usize,
        total: usize,
        preload_indices: &[usize],
    ) {
        let mut tasks: Vec<(usize, PathBuf)> = Vec::new();
        if let Some(dir) = &self.navigation.dir_list {
            for &i in preload_indices {
                if i == current_index {
                    continue;
                }
                let Some(p) = dir.get(i) else { continue };
                if self.navigation.image_cache.contains(p) {
                    continue;
                }
                tasks.push((i, p.to_path_buf()));
            }
        }
        if !tasks.is_empty()
            && let Some(preloader) = &mut self.navigation.preloader
        {
            preloader.request_neighbor_preload(tasks, current_index, total);
        }
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

    /// EXIF metadata of the currently displayed image, if any. The cache
    /// is the source of truth: navigation always insert-then-display, so
    /// the current image is always cache-resident by the time we render.
    pub(crate) fn current_exif(&self) -> Option<&decoding::ExifMetadata> {
        let dir = self.navigation.dir_list.as_ref()?;
        let entry = self.navigation.image_cache.peek(dir.current())?;
        entry.exif.as_deref()
    }

    /// True iff the currently displayed image has any EXIF metadata. Used
    /// by the shared state snapshot so MCP clients can tell why the EXIF
    /// panel might not be rendering even with `exif_visible == true`.
    pub(crate) fn current_image_has_exif(&self) -> bool {
        self.current_exif().is_some()
    }

    /// Recompute the histogram's hover bin from the cached cursor position.
    /// Requests a redraw and writes shared state only when the bin actually
    /// changed — keeps the render-on-demand model intact during idle mouse
    /// motion outside the histogram rect.
    pub(crate) fn update_histogram_hover(&mut self) {
        if !self.histogram.visible {
            if self.histogram.hover_bin.is_some() {
                self.histogram.hover_bin = None;
                self.request_redraw();
                self.update_shared_state();
            }
            return;
        }
        // Compute the rect deterministically from current layout — no
        // dependency on a prior `Renderer::render` call. This way an MCP
        // `set_cursor_position` arriving before the first frame still
        // produces a hover bin.
        let new_bin = self.renderer.as_ref().and_then(|r| {
            let rect =
                histogram::overlay::plot_rect_for(r.logical_width(), self.content_offset_y());
            rect.bin_at(
                Logical(self.last_mouse_pos.0.0 as f32),
                Logical(self.last_mouse_pos.1.0 as f32),
            )
        });
        if new_bin != self.histogram.hover_bin {
            self.histogram.hover_bin = new_bin;
            self.request_redraw();
            self.update_shared_state();
        }
    }

    /// Build the title and zoom-readout strings shown in the title-bar strip. The title is
    /// `"{i} / {n} – {filename}"` (with the folder-position prefix) or the bare filename for a
    /// single image; the zoom readout is `"{pct}%"`. One source for both the native labels
    /// (`window::set_titlebar_text`) and the glyphon pills (`build_text_overlay`).
    fn titlebar_text(&self) -> Option<(String, String)> {
        let dir = self.navigation.dir_list.as_ref()?;

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

        Some((title, zoom_text))
    }

    /// Build text blocks for the header overlay. Pills are computed from actual text
    /// measurements during prepare() — no manual rect computation needed here.
    ///
    /// When the title bar is on, the title/zoom readout is drawn by native `NSTextField`
    /// labels (`window::set_titlebar_text`) that ride inside the glass strip and auto-contrast
    /// in light/dark mode, so glyphon builds neither here. When the title bar is off, the
    /// text floats over the image, so glyphon draws it with a dark pill for contrast. The
    /// centered "Loading…" overlay stays glyphon in both cases.
    fn build_text_overlay(&self) -> Vec<text::TextBlock> {
        let Some(rend) = &self.renderer else {
            return Vec::new();
        };
        let Some((title, zoom_text)) = self.titlebar_text() else {
            return Vec::new();
        };

        let logical_width = rend.logical_width();

        let pill_color: [f32; 4] = [0.0, 0.0, 0.0, 0.55];

        // With the title bar on, the native `NSTextField` labels own the title/zoom readout
        // (see `window::set_titlebar_text`), so glyphon draws neither. With the title bar off,
        // the text floats over the image and glyphon draws it with a dark pill for contrast.
        let mut blocks: Vec<text::TextBlock> = Vec::new();
        if !self.title_bar {
            let pad_x = Logical(8.0_f32);
            let pad_y = Logical(4.0_f32);
            let radius = Logical(5.0_f32);
            let title_x = Logical(88.0_f32); // Right of the traffic lights (nudged in 8px from the edge)
            let title_y = Logical(4.0_f32); // Sits in the title-bar strip, a touch below the top
            let zoom_margin = Logical(7.0_f32); // Equidistant from top and right edge

            // The zoom pill is right-aligned: x = the right edge of the pill.
            let zoom_right_edge = logical_width - zoom_margin;
            let zoom_budget = Logical(70.0_f32); // space reserved for zoom pill (for title truncation)
            let gap = Logical(12.0_f32); // minimum space between title and zoom pills
            let title_max_render =
                logical_width - title_x - zoom_budget - pad_x * 2.0 - zoom_margin - gap;

            let title_block = text::TextBlock::new(title, title_x + pad_x, title_y + pad_y)
                .bold()
                .max_render_width(title_max_render)
                .pill(pill_color, pad_x, pad_y, radius);
            let zoom_block = text::TextBlock::new(zoom_text, zoom_right_edge, title_y + pad_y)
                .bold()
                .align_right()
                .pill(pill_color, pad_x, pad_y, radius);
            blocks.push(title_block);
            blocks.push(zoom_block);
        }

        // Centered "Loading..." overlay during a pending navigation target.
        // Styled like the title pill but larger — system font, bigger
        // font size, bigger corner radius to match. The `align_center`
        // flag measures the actual shaped text width at prepare time and
        // repositions the block so the text is truly centered on `x`;
        // the pill follows the text.
        if self.navigation.pending_current.is_some() {
            let logical_height = rend.logical_height();
            let loading_font_size = 14.0_f32;
            let loading_line_height = 18.0_f32;
            let loading_pad_x = Logical(11.0_f32);
            let loading_pad_y = Logical(5.0_f32);
            let loading_radius = Logical(7.0_f32);
            let center_x = Logical(logical_width.0 / 2.0);
            let center_y = Logical((logical_height.0 - loading_line_height) / 2.0);
            let mut loading = text::TextBlock::new("Loading...", center_x, center_y);
            loading.font_size = loading_font_size;
            loading.line_height = loading_line_height;
            loading = loading.bold().align_center().pill(
                pill_color,
                loading_pad_x,
                loading_pad_y,
                loading_radius,
            );
            blocks.push(loading);
        }

        blocks
    }

    /// Drain preloader responses, cache the results, and if the pending
    /// navigation target just arrived, render it immediately.
    fn poll_preloader(&mut self) {
        // Drain responses into an owned Vec so we can release the
        // preloader borrow before calling `display_from_cache` (which needs
        // `&mut self`).
        let responses: Vec<preloader::PreloadResponse> = if let Some(p) = &self.navigation.preloader
        {
            p.response_rx.try_iter().collect()
        } else {
            return;
        };

        let mut neighbor_arrived = false;
        for response in responses {
            match response {
                preloader::PreloadResponse::Ready {
                    index,
                    path,
                    image,
                    decode_duration,
                    file_name,
                } => {
                    if let Some(p) = &mut self.navigation.preloader {
                        p.mark_complete(&path);
                    }
                    let evicted =
                        self.navigation
                            .image_cache
                            .insert(path, image, decode_duration, file_name);
                    self.log_evictions(evicted, "LRU");
                    if self.navigation.pending_current != Some(index) {
                        neighbor_arrived = true;
                    }
                    if self.navigation.pending_current == Some(index) {
                        self.navigation.pending_current = None;
                        self.display_from_cache(index);
                        // Title was "Loading…" — swap to the final title.
                        if let Some(dir) = &self.navigation.dir_list
                            && let Some(win) = &self.window
                        {
                            window::set_title_keeping_buttons(
                                win,
                                &window::window_title_with_position(
                                    dir.current(),
                                    dir.current_index(),
                                    dir.len(),
                                ),
                            );
                        }
                        if let Some(requested_at) = self.request_times.remove(&index) {
                            let elapsed = requested_at.elapsed().as_millis();
                            log::info!("Image #{index} displayed after {elapsed}ms");
                        } else {
                            log::info!("Image #{index} displayed");
                        }
                        #[cfg(target_os = "macos")]
                        {
                            let had_placeholder = self.placeholder_active;
                            self.placeholder_active = false;
                            self.record_preview_event(
                                "primary-arrived",
                                format!("index={index} had_placeholder={had_placeholder}"),
                            );
                            self.on_primary_decode_settled();
                        }
                        // Now that the user-visible target has arrived,
                        // queue the neighbors for background pre-decode.
                        // Deferring this until now keeps the FIFO channel
                        // small during rapid navigation so the latest
                        // priority-0 isn't piled behind stale neighbors.
                        if self.navigation.preload_neighbors {
                            let (total, neighbors) = if let Some(dir) = &self.navigation.dir_list {
                                (
                                    dir.len(),
                                    dir.preload_range(
                                        preloader::preload_count(),
                                        self.navigation.last_direction,
                                        self.navigation.loop_navigation,
                                    ),
                                )
                            } else {
                                (0, Vec::new())
                            };
                            if !neighbors.is_empty() {
                                self.submit_neighbor_preload(index, total, &neighbors);
                            }
                        }
                        self.update_shared_state();
                    }
                }
                preloader::PreloadResponse::Failed {
                    index,
                    path,
                    reason,
                } => {
                    if let Some(p) = &mut self.navigation.preloader {
                        p.mark_complete(&path);
                    }
                    log::debug!(
                        "Preload response: failed [{index}] {}: {reason}",
                        path.display()
                    );
                    if self.navigation.pending_current == Some(index) {
                        self.navigation.pending_current = None;
                        log::error!(
                            "Failed to decode current image {}: {reason}",
                            path.display()
                        );
                        if let Some(win) = &self.window {
                            window::set_title_keeping_buttons(win, &format!("Prvw - {reason}"));
                        }
                        #[cfg(target_os = "macos")]
                        self.on_primary_decode_settled();
                    }
                }
                preloader::PreloadResponse::Cancelled { index: _, path } => {
                    if let Some(p) = &mut self.navigation.preloader {
                        p.mark_complete(&path);
                    }
                }
                preloader::PreloadResponse::Salvaged {
                    index,
                    path,
                    image,
                    decode_duration,
                    file_name,
                } => {
                    // A cancelled JPEG/generic decode finished anyway. Keep it
                    // only if it's still in the hot window and not already
                    // cached; otherwise the respect-resources policy says drop
                    // it rather than let an out-of-window image squat in RAM.
                    // Deliberately not used to satisfy `pending_current`: the
                    // prioritized fresh decode owns the user-visible target.
                    let in_window = self.current_window_keep_paths().iter().any(|p| p == &path);
                    let already_cached = self.navigation.image_cache.contains(&path);
                    if in_window && !already_cached {
                        log::debug!("Salvaged decode [{index}] {} into cache", path.display());
                        let evicted = self.navigation.image_cache.insert(
                            path,
                            image,
                            decode_duration,
                            file_name,
                        );
                        self.log_evictions(evicted, "LRU");
                        neighbor_arrived = true;
                    } else {
                        log::debug!(
                            "Dropped salvaged decode [{index}] {} (in_window={in_window}, already_cached={already_cached})",
                            path.display()
                        );
                    }
                }
                preloader::PreloadResponse::Preview { index, path, image } => {
                    // RAW embedded-JPEG preview: show it as a soft placeholder
                    // only while we're still waiting on THIS target's full
                    // develop. If a newer nav moved on (index no longer
                    // pending), or the full image already landed, drop it.
                    // macOS-only display (see `display_raw_preview_placeholder`).
                    #[cfg(target_os = "macos")]
                    if self.navigation.pending_current == Some(index) {
                        log::debug!(
                            "Showing RAW preview placeholder [{index}] {}",
                            path.display()
                        );
                        self.display_raw_preview_placeholder(index, image);
                    }
                    #[cfg(not(target_os = "macos"))]
                    let _ = (index, path, image);
                }
            }
        }
        // Background neighbour preloads insert into the cache without
        // touching `pending_current`. Mirror the new `cache_indices` into
        // shared state so the QA server / MCP clients see them.
        if neighbor_arrived {
            self.update_shared_state();
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

    /// Return the AppKit `NSWindow.windowNumber` of the main viewer window.
    /// Used by the debug-only `screenshot_window` MCP tool, which shells out to
    /// `/usr/sbin/screencapture -l <number>` to capture the window as the user sees
    /// it (overlays, vibrancy, title bar, the lot). Returns `None` if the window
    /// hasn't been created yet or the number is non-positive.
    #[cfg(all(debug_assertions, target_os = "macos"))]
    pub(crate) fn main_window_number(&self) -> Option<u32> {
        use objc2::msg_send;
        use objc2_app_kit::NSWindow;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let win = self.window.as_ref()?;
        let RawWindowHandle::AppKit(handle) = win.window_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: winit gives us a valid `NSView*`. `[NSView window]` returns the
        // owning NSWindow (or nil if the view is detached). `[NSWindow windowNumber]`
        // is a non-negative integer for visible windows; anything ≤ 0 means "no
        // assigned number yet".
        let ns_view = handle.ns_view.as_ptr() as *const objc2::runtime::AnyObject;
        let ns_win: *const NSWindow = unsafe { msg_send![ns_view, window] };
        if ns_win.is_null() {
            return None;
        }
        let number: isize = unsafe { msg_send![ns_win, windowNumber] };
        if number <= 0 {
            None
        } else {
            Some(number as u32)
        }
    }

    /// Pop up the right-click context menu (currently just "Copy image") at the cursor.
    /// No-op when no image is open. The selected item posts a `MenuEvent` picked up by
    /// `handle_menu_event` on the next `about_to_wait`, same path as the menu bar.
    #[cfg(target_os = "macos")]
    fn show_image_context_menu(&self) {
        use muda::ContextMenu;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let Some(app_menu) = &self.app_menu else {
            return;
        };
        if self.navigation.dir_list.is_none() {
            return;
        }
        let Some(win) = &self.window else {
            return;
        };
        let Ok(RawWindowHandle::AppKit(handle)) = win.window_handle().map(|h| h.as_raw()) else {
            return;
        };
        let ns_view = handle.ns_view.as_ptr() as *const std::ffi::c_void;
        // SAFETY: winit gives us a valid `NSView*` for the main window. A `None` position
        // tells muda to use the current mouse location.
        unsafe {
            app_menu
                .context_menu
                .show_context_menu_for_nsview(ns_view, None);
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

    /// Print the current image via the system print sheet. Mirrors the NSWindow-pointer
    /// extraction used by the About/Settings dialogs, then hands off to the print module.
    #[cfg(target_os = "macos")]
    fn print_current_image(&mut self) {
        use objc2::msg_send;
        use objc2_app_kit::NSWindow;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let Some(path) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.current().to_path_buf())
        else {
            log::debug!("Print: no image open");
            return;
        };

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

        if parent_ptr.is_null() {
            log::warn!("Print: no viewer window to attach the print sheet to");
            return;
        }

        // SAFETY: parent_ptr is a valid, live NSWindow* for the duration of this call.
        let parent_window = unsafe { &*parent_ptr };
        self._active_print = crate::platform::macos::print::print_image_file(&path, parent_window);
        if self._active_print.is_some() {
            log::info!("Printing image: {}", path.display());
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
        if event.id() == &app_menu.ids.histogram {
            // CheckMenuItem auto-toggles on click. The toggle command
            // re-syncs the checkmark afterward; we just fire it here.
            let _ = self
                .event_loop_proxy
                .send_event(AppCommand::ToggleHistogram);
            return;
        }
        if event.id() == &app_menu.ids.exif_info {
            let _ = self.event_loop_proxy.send_event(AppCommand::ToggleExifInfo);
            return;
        }
        if event.id() == &app_menu.ids.loop_navigation {
            let _ = self
                .event_loop_proxy
                .send_event(AppCommand::ToggleLoopNavigation);
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
                // Check-only: log if an update is available, but don't download yet — an
                // admin-password prompt while the user is on the onboarding screen would be
                // invasive. The actual install fires from `initialize_viewer` once they open
                // a file.
                #[cfg(target_os = "macos")]
                if settings::Settings::load().auto_update {
                    updater::check_only();
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

        // Debounced navigation: fire once the quiet window passes. Between
        // queue and fire we hold `ControlFlow::WaitUntil(deadline)`, so winit
        // wakes us exactly at the deadline.
        if let Some(deadline) = self.navigation.nav_deadline
            && Instant::now() >= deadline
        {
            self.flush_pending_nav();
        }

        // Slideshow auto-advance: fire when the dwell timer elapses, but only
        // once the next image is decoded and the current one is fully shown.
        // Otherwise hold here; a preloader completion wakes us to re-check, so
        // the switch is always instant and clean (no "Loading…" flash) even
        // when a big image decodes slower than the per-image interval.
        if self.slideshow.running
            && let Some(deadline) = self.slideshow.next_advance
            && Instant::now() >= deadline
            && (self.slideshow_ready_to_advance()
                || Instant::now() >= deadline + slideshow::MAX_HOLD)
        {
            self.slideshow_advance();
        }

        // Crossfade animation: ramp the fade factor each frame until done.
        self.drive_crossfade();

        // Browse-mode tree loading overlay: reveal it once a scan has been pending past the 1s
        // delay, hide it when scans finish. No-op outside browse mode / off macOS.
        #[cfg(target_os = "macos")]
        self.browser.refresh_loading_overlay();

        // Browse-mode grid: feed the collection view's current visible range to the thumbnail
        // scheduler/cache and pump generation. Native scrolling routes through this run loop, so
        // `about_to_wait` fires after a scroll; the scheduler dedups already-cached/in-flight
        // indices, so this is cheap when nothing changed. No-op outside browse mode / off macOS.
        #[cfg(target_os = "macos")]
        if self.browser.is_browse() {
            self.browser.grid_pump_visible_range();
        }

        // Set the next wakeup from whatever's still pending (nav debounce,
        // slideshow timer, crossfade frames, tree-scan overlay), or idle.
        self.schedule_wakeup(event_loop);
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
                    // The glass frame mask doesn't autoresize; rebuild it for the new size.
                    window::apply_glass_frame_mask(win);
                    // The traffic lights stay nudged across the relayout via the swizzled
                    // frame setters (see `window::register_traffic_light_keeper`).
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
                // On a fullscreen transition, re-decide the zoom (fit vs actual size) for the
                // new viewport mode: entering fullscreen drops auto-fit's grip, so the
                // fit/enlarge rules apply afresh. A plain manual resize keeps the user's zoom
                // and only re-clamps the floor.
                let now_fullscreen = self.is_fullscreen();
                if now_fullscreen != self.was_fullscreen {
                    self.was_fullscreen = now_fullscreen;
                    self.apply_initial_zoom();
                } else {
                    // Recalculate zoom floor — image-to-window ratio changed
                    self.update_min_zoom();
                }
                if let Some(renderer) = &self.renderer {
                    renderer.update_transform(&self.zoom.view.transform());
                }
                self.request_redraw();
                self.update_shared_state();
            }

            WindowEvent::RedrawRequested if self.needs_redraw => {
                log::trace!("Rendering frame");
                let mut text_blocks = self.build_text_overlay();
                let offset = self.content_offset_y();

                // When the title bar is on (and not fullscreen), the native title/zoom labels
                // in the glass strip own the readout. They auto-contrast in light/dark, so they
                // replace the glyphon pills (which `build_text_overlay` skips in this case).
                #[cfg(target_os = "macos")]
                if self.title_bar
                    && !self.is_fullscreen()
                    && let Some(win) = &self.window
                    && let Some((title, zoom_text)) = self.titlebar_text()
                {
                    window::set_titlebar_text(win, &title, &zoom_text);
                }
                // The histogram and EXIF overlays anchor to a fixed top inset (the title-bar
                // height) rather than the content offset, so they stay put when the title bar
                // is off instead of riding up over the zoom readout.
                let overlay_offset = Logical(TITLE_BAR_HEIGHT);

                // Build the histogram overlay if it's visible and we have data. The
                // produced `HistogramDrawCall` borrows immutably from `self.histogram.data`
                // for the duration of the render call.
                let mut standalone_pills: Vec<crate::render::text::StandalonePill> = Vec::new();
                let logical_width = self.renderer.as_ref().map(|r| r.logical_width());
                let histogram_call: Option<crate::render::renderer::HistogramDrawCall<'_>> = if self
                    .histogram
                    .visible
                    && let (Some(width), Some(data)) = (logical_width, self.histogram.data.as_ref())
                {
                    let build = histogram::overlay::build(
                        data,
                        self.histogram.hover_bin,
                        width,
                        overlay_offset,
                    );
                    standalone_pills.extend(build.pills);
                    for tb in build.text_blocks {
                        text_blocks.push(tb);
                    }
                    Some(build.draw_call)
                } else {
                    None
                };

                // EXIF info overlay. Hidden when the user toggled it off OR
                // when the current image carries no EXIF — even toggled-on,
                // a wall of "n/a" rows would just be noise.
                if self.exif_overlay.visible
                    && let Some(width) = logical_width
                    && let Some(metadata) = self.current_exif()
                {
                    let build = exif_overlay::overlay::build(
                        metadata,
                        width,
                        overlay_offset,
                        self.histogram.visible,
                    );
                    standalone_pills.extend(build.pills);
                    for tb in build.text_blocks {
                        text_blocks.push(tb);
                    }
                }

                let rendered = self.renderer.as_mut().is_some_and(|renderer| {
                    renderer.render(&text_blocks, &standalone_pills, histogram_call, offset)
                });
                if rendered {
                    self.needs_redraw = false;
                } else if let Some(win) = &self.window {
                    win.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                // Branch by mode: winit keeps delivering keyboard input even with the native
                // browse split view up, so browse mode gets its own key mapping. Image mode is
                // unchanged. See `docs/specs/image-browser.md` → "Input architecture".
                let key = event.logical_key.as_ref();
                let command = if self.browser.is_browse() {
                    input::browse_key_to_command(key, &self.modifiers)
                } else {
                    input::key_to_command(key, &self.modifiers)
                };
                if let Some(command) = command {
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
                        // Navigate: scroll down = next, scroll up = previous.
                        // Debounced so a wheel spin collapses to one jump.
                        let forward = scroll_y < 0.0;
                        self.execute_command(event_loop, AppCommand::NavigateDebounced(forward));
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
                self.update_histogram_hover();

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
                        // A double-click on the title bar zooms the window like any native
                        // macOS app (our content view covers the title bar, so AppKit never
                        // sees it — we forward it). Anywhere on the image toggles the fit.
                        #[cfg(target_os = "macos")]
                        if self.pointer_in_title_bar() {
                            if let Some(win) = &self.window {
                                window::zoom_window(win);
                            }
                        } else {
                            self.execute_command(event_loop, AppCommand::ToggleFit);
                        }
                        #[cfg(not(target_os = "macos"))]
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

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                #[cfg(target_os = "macos")]
                self.show_image_context_menu();
            }

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
