//! `App` — the core viewer state and event-loop integration.
//!
//! Owns the window, renderer, preloader, image cache, and all user-facing settings.
//! Implements `winit::ApplicationHandler` and dispatches every `AppCommand` through
//! `execute_command` (see `executor.rs`).

mod executor;
mod previews_hook;
mod shared_state;

pub(crate) use shared_state::SharedAppState;

use crate::color::display_profile;
use crate::commands::{self, AppCommand};
use crate::diagnostics::NavigationRecord;
use crate::navigation::{directory, preloader};
use crate::pixels::{
    Logical, from_logical_pos, from_logical_size, from_physical_size, to_logical_pos,
    to_logical_size,
};
use crate::render::{renderer, text};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use crate::updater;
use crate::{
    TITLE_BAR_HEIGHT, color, decoding, exif_overlay, histogram, input, menu, navigation, qa,
    scroll, settings, slideshow, window, zoom,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
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

/// Why image mode is showing no image. Both draw a clean black canvas with one centered
/// overlay; only the words differ, because only the way out differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EmptyState {
    /// Nothing has been opened yet: a launch that named no file and no folder. Off macOS only,
    /// since macOS waits for Finder's Apple Event and shows onboarding meanwhile
    /// (`launch::waits_for_a_file`). The overlay says how to open something, and a click
    /// anywhere does it.
    NothingOpen,
    /// The active folder has no images: the last one was deleted under live folder sync, or a
    /// folder argument held none. On the live-sync route the folder stays watched, so an image
    /// appearing there opens by itself; on the launch route nothing was ever watched.
    NoImages,
}

impl EmptyState {
    /// The name `/state` reports it under, for QA and the E2E suite.
    pub(crate) fn name(self) -> &'static str {
        match self {
            EmptyState::NothingOpen => "nothing_open",
            EmptyState::NoImages => "no_images",
        }
    }

    /// The centered overlay's text.
    fn overlay(self) -> &'static str {
        match self {
            // Draft copy, for David to review. It opens the way `docs/specs/windows-ui-design.md`
            // asks ("Open an image to start") and then names every way in, because Linux has no
            // menu bar to advertise File → Open and a Windows user reaching for the keyboard
            // shouldn't have to find one. One line, because the pill behind it is one line high
            // (`render::text`). M6 is where the icon and the default-handler line join it.
            EmptyState::NothingOpen => {
                if cfg!(target_os = "macos") {
                    "Open an image to start: drop one here, click, or press \u{2318}O"
                } else {
                    "Open an image to start: drop one here, click, or press Ctrl+O"
                }
            }
            EmptyState::NoImages => "(No images)",
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
    /// A directory passed on the CLI: launch straight into browse mode with this folder revealed +
    /// selected in the tree (instead of image mode). `None` for an image-file or no-argument
    /// launch. Consumed by `initialize_viewer`.
    pub(crate) launch_directory: Option<PathBuf>,
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
    /// Paths from a drop in progress. winit reports one `DroppedFile` per file with no event
    /// marking the end of the batch, so they pile up here and `about_to_wait` — which runs once
    /// the whole batch is drained — opens them as one request. See `App::open_dropped`.
    pub(crate) pending_drops: Vec<PathBuf>,
    /// Whether a drag is hovering files over the window right now, which puts a "Drop to open"
    /// pill on the canvas. Set by `HoveredFile`, cleared by `HoveredFileCancelled` and by the
    /// drop itself (winit sends no cancel once a drop lands).
    pub(crate) files_hovering: bool,
    pub(crate) last_mouse_pos: (Logical<f64>, Logical<f64>),
    pub(crate) last_click_time: Option<Instant>,
    /// What a scroll event means here: the platform's zoom modifier, and the running conversion
    /// from raw deltas to zoom steps and images. See `crate::scroll`.
    pub(crate) scroll: scroll::Scroll,
    pub(crate) needs_redraw: bool,
    /// Whether the centered "Loading…" overlay is currently drawn. It only appears once the image
    /// we're waiting on has taken longer than `navigation::LOADING_OVERLAY_DELAY`, so a local file
    /// never flashes it. Recomputed in `about_to_wait`; a change there asks for a redraw.
    pub(crate) loading_overlay_visible: bool,
    /// Set by a slideshow auto-advance to request that the next image display
    /// crossfades from the current one. Consumed (and cleared) by
    /// `display_from_cache`; cleared on a cache miss so only instant,
    /// already-cached advances crossfade.
    pub(crate) pending_crossfade: bool,
    /// The current display's scale factor: 2.0 on a Retina Mac, 1.0, 1.25, 1.5, or 1.75 on a
    /// typical Windows display. Taken from the window the moment it exists and updated on every
    /// `ScaleFactorChanged`, so it always names the monitor the window is on.
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
    /// Live folder-sync watcher (FSEvents-backed). Watches the active folder — the current image's
    /// folder in image mode — so adds/modifies/deletes reflect without a manual refresh. `None`
    /// when the platform watcher couldn't start (live sync just stays off). See
    /// `crate::folder_watch` and `App::retarget_active_folder_watch`.
    pub(crate) folder_watcher: Option<crate::folder_watch::FolderWatcher>,
    /// The folder currently being watched by `folder_watcher`, so a re-target can unwatch the old
    /// one before watching the new one. `None` before the first watch or in the empty state.
    pub(crate) watched_folder: Option<PathBuf>,
    /// The folders whose watch the `folder_watch` worker has actually applied, as reported by
    /// `AppCommand::WatchedFoldersChanged`. Lags `watched_folder` / `watched_tree_folders` (those
    /// are requests; this is what FSEvents is really delivering for) and exists to be exposed as
    /// `watched_folders` in shared state.
    pub(crate) armed_watch_folders: Vec<PathBuf>,
    /// Why image mode is showing no image, or `None` while it's showing one. Drives a clean
    /// black canvas plus one centered overlay. Cleared when an image is displayed (leaving the
    /// folder, opening a file, or one appearing in the watched folder).
    pub(crate) empty_state: Option<EmptyState>,
    /// The one worker that reads directories for the whole app (image mode, the browse grid, the
    /// browse tree, live sync). `None` until the viewer initializes. See `crate::folder_scan`.
    pub(crate) folder_scanner: Option<crate::folder_scan::FolderScanner>,
    /// The folder whose scan the browse grid is waiting on, so `FolderScanned` for it repopulates
    /// the grid. `None` when the grid isn't waiting on anything.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(crate) pending_grid_listing: Option<PathBuf>,
    /// The folder whose scan live sync is waiting on, so `FolderScanned` for it runs the
    /// add/remove/delete-current diff. `None` when no change is outstanding.
    pub(crate) pending_rescan: Option<PathBuf>,
    /// Paths flagged `Modify` by the latest `FolderChanged`, carried across the async re-scan so
    /// `apply_folder_rescan` can re-decode a modified currently-displayed image. Cleared on apply.
    pub(crate) pending_modified: Vec<PathBuf>,
    /// Browse-mode tree folders currently watched for subdirectory changes (`Part B`): the roots
    /// (always watched once browse is built) plus every folder the user has expanded. Bounded to
    /// what's visible — never the whole disk. A folder is added on expand / at root setup and
    /// removed on collapse (`watch_tree_folder` / `unwatch_tree_folder`). A `FolderChanged` for one
    /// of these reloads that tree node's children. Distinct from `watched_folder` (the image-list
    /// watch); a folder can be in both. Only where there's a native tree.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(crate) watched_tree_folders: Vec<PathBuf>,

    // ── Preview placeholder tracking ─────────────────────────────
    /// True while the image texture holds a preview placeholder
    /// (uploaded on a cache-miss before the full decode arrives).
    /// Cleared when `display_from_cache` runs with the full image.
    pub(crate) placeholder_active: bool,
    /// Monotonic start time for event-timeline timestamps.
    pub(crate) app_start: Instant,
    /// Ring buffer of recent preview-lifecycle events. Mirrored to
    /// `SharedAppState` on every `update_shared_state` so MCP clients
    /// can query the timeline after the fact. Capped at 64.
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
        launch_directory: Option<PathBuf>,
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
            launch_directory,
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
            previews: crate::previews::State::new(),
            title_bar: initial_settings.title_bar,
            raw_flags: initial_settings.raw,
            edr_headroom: 1.0,
            current_image_is_hdr: false,
            modifiers: ModifiersState::empty(),
            drag_start: None,
            pending_drops: Vec::new(),
            files_hovering: false,
            last_mouse_pos: (Logical(0.0), Logical(0.0)),
            last_click_time: None,
            scroll: scroll::Scroll::for_host(),
            needs_redraw: false,
            loading_overlay_visible: false,
            pending_crossfade: false,
            // Neutral until there's a window to ask (`initialize_viewer`). Anything else is a
            // guess at one platform's hardware, and nothing reads this before then anyway.
            scale_factor: 1.0,
            was_fullscreen: false,
            shared_state,
            event_loop_proxy,
            _qa_handle: None,
            folder_watcher: None,
            watched_folder: None,
            armed_watch_folders: Vec::new(),
            empty_state: None,
            folder_scanner: None,
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            pending_grid_listing: None,
            pending_rescan: None,
            pending_modified: Vec::new(),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            watched_tree_folders: Vec::new(),
            placeholder_active: false,
            app_start: Instant::now(),
            preview_events: std::collections::VecDeque::with_capacity(64),
            request_times: std::collections::HashMap::new(),
        }
    }

    /// Compute the content offset based on the title_bar setting and fullscreen state.
    ///
    /// macOS only: the strip exists because the window draws its content behind a transparent
    /// title bar (`window::configure_macos_window`). A Win32 or X11 client area starts below
    /// the decorations, so reserving space there would leave a black band under a title bar
    /// that was never in the way. `parity::setting_keys` says the same thing about
    /// `SettingKey::TitleBar`.
    fn content_offset_y(&self) -> Logical<f32> {
        if cfg!(target_os = "macos") && self.title_bar && !self.is_fullscreen() {
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
        if self.color.match_display
            && let Some(icc) = display_profile::display_icc(window)
        {
            return icc;
        }
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

        // Create the native menu bar. `None` on a platform that has none. Before the renderer,
        // because a Windows menu bar takes its height out of the client area: attaching it
        // after the wgpu surface exists would size the surface twice.
        self.app_menu = menu::create_menu_bar(&win);

        // Create renderer (wgpu surface must be created here, in resumed())
        self.renderer = Some(renderer::Renderer::new(win.clone()));

        // Set up title bar area before any image display
        self.zoom.view.set_content_offset_y(self.content_offset_y());

        // Configure ICC color management based on settings
        self.color.display_icc = self.effective_display_icc(&win);
        if let Some(renderer) = &mut self.renderer {
            renderer.set_display_icc(self.color.display_icc.clone());
        }
        // Record which display that profile came from, so the first nudge of the window doesn't
        // read as a screen change and flush the cache for nothing.
        if let Some(monitor) = display_profile::current_monitor(&win) {
            self.color.monitors.moved_to(monitor);
        }

        // Query the display's EDR headroom. 1.0 on SDR displays (so the
        // RAW decoder stays on the Phase 4 RGBA8 path, bit-identical).
        // XDR and OLED displays return >1.0 which promotes RAWs to the
        // RGBA16F + filmic-4×-shoulder path.
        self.edr_headroom = self.query_edr_headroom();
        log::info!("Display EDR headroom: {:.2}", self.edr_headroom);
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
            // Watch AppKit's fullscreen transitions, so a request made during one isn't
            // dropped and a transition we didn't start still leaves our state right.
            window::register_fullscreen_observer(&win);
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

        // Build the navigation list. Nothing here reads the directory: on a network share that
        // takes tens of seconds, and until it finished nothing would paint. The shared scanner
        // does the read, and the list it produces lands via `AppCommand::FolderScanned` (see
        // `install_scanned_folder`).
        let initial_sort_by = settings::Settings::load().sort_by;
        // The browse grid lists folder images in the same order, so opening a grid item lands on
        // the matching image-mode index.
        self.browser.set_sort_by(initial_sort_by);
        let launch_directory = self.launch_directory.take();
        // The folder this launch needs read, if any. `None` for a directory launch that boots
        // into browse mode (the tree asks for its own folders) and for an explicit multi-file
        // open, where the named files ARE the navigation set.
        let mut folder_to_scan: Option<PathBuf> = None;
        self.navigation.dir_list = if let Some(dir) = &launch_directory {
            // A folder argument. A platform with a browser boots into browse mode at it
            // (handled at the end of this function), so there's no list and no initial image —
            // the user opens one from the grid. Linux has no browser, and a window with no image
            // and no way to get one is the defect M1 step 1 exists to fix, so there the folder
            // becomes an image-mode playlist instead: its images in the user's sort order,
            // starting at the first. The scanner reads it like every other folder, so the list
            // arrives in `install_scanned_folder`, and a folder with no images lands in the
            // "(No images)" empty state from there.
            if !cfg!(any(target_os = "macos", target_os = "windows")) {
                folder_to_scan = Some(dir.clone());
            }
            None
        } else if let Some(files) = self.explicit_files.take() {
            // The list sorts; the image that opens is the one named first on the command line
            // (`self.file_path`), so the list has to sit on it rather than on whatever sorts
            // first.
            Some(directory::DirectoryList::from_explicit(
                files,
                initial_sort_by,
                Some(&self.file_path),
            ))
        } else if self.file_path.as_os_str().is_empty() {
            // A launch that named nothing. The window still comes up; the empty state below
            // says how to open something.
            None
        } else {
            folder_to_scan = self.file_path.parent().map(Path::to_path_buf);
            Some(directory::DirectoryList::provisional(
                &self.file_path,
                initial_sort_by,
            ))
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

        // Start the live folder-sync watcher and the shared folder scanner BEFORE the first
        // display, so the watch is already live while a slow scan runs (a file added meanwhile
        // isn't missed) and the scan overlaps the image's own read+decode instead of following it.
        self.folder_watcher =
            crate::folder_watch::FolderWatcher::start(self.event_loop_proxy.clone());
        self.folder_scanner = Some(crate::folder_scan::FolderScanner::start(
            self.event_loop_proxy.clone(),
        ));

        // Ask for the folder this launch needs. Until it lands, `dir_list` is the provisional
        // single-image list (or nothing at all, for a folder argument with no browser to show it)
        // and navigation stays put. A directory launch into browse mode asks for its own folders
        // below.
        if let Some(folder) = folder_to_scan {
            self.navigation.scan_pending = Some(navigation::PendingScan {
                folder: folder.clone(),
                landing: if launch_directory.is_some() {
                    navigation::ScanLanding::PlayFromTop
                } else {
                    navigation::ScanLanding::KeepOpenImage
                },
            });
            self.request_folder_scan(folder);
        }
        self.retarget_active_folder_watch();

        // Display the initial image. Every format takes the async path (mirrors cache-miss
        // navigation): the preloader owns the read+decode, so the window paints now and the image
        // lands when it's ready. A directory launch has no initial image — browse mode opens
        // below, or the scan lands and plays the folder from the top.
        if !self.file_path.as_os_str().is_empty() {
            let initial_path = self.file_path.clone();
            self.display_current_async(&initial_path);
            // An explicit file list is already the whole navigation set, so its neighbors can warm
            // right away. A scanned folder warms in `install_scanned_folder` instead.
            if self.navigation.scan_pending.is_none() {
                self.warm_initial_neighbors();
            }
        } else if launch_directory.is_none() {
            // Nothing was named at all. macOS never reaches this (it waits for Finder's Apple
            // Event instead, see `launch::waits_for_a_file`); everywhere else this is the
            // window that used to not exist.
            self.empty_state = Some(EmptyState::NothingOpen);
        }

        // Pause the preview scheduler while the initial primary decode is
        // running (the async RAW path leaves `pending_current` set). The full
        // decode's arrival in `poll_preloader` resumes it.
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

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if settings::Settings::load().auto_update {
            updater::check_on_launch();
        }

        // Directory launch: boot straight into browse mode with the folder revealed + selected in
        // the tree and its images listed in the grid. Reuses the browse-open reveal path; the
        // listing then focuses the grid (or the tree for an empty folder, per `grid_folder_listed`
        // / `enter_browse`). Done after the window/renderer/menu/preloader are up so the swap and
        // the eventual image reveal have everything they need.
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(dir) = launch_directory {
            log::info!("Directory launch: opening browse mode at {}", dir.display());
            self.browser.enter_browse(&win);
            // No current image to round-trip to (nothing was opened); preselect the folder's first
            // image (`reveal_to_folder` with `None` → the grid picks index 0).
            self.browser.reveal_to_folder(&dir, None);
            self.set_browse_menu_label();
            // Live folder sync (Part B): the browser (and its tree) now exist — watch the roots.
            self.watch_tree_roots();
        }

        self.request_redraw();
    }

    /// Show the image at the directory cursor asynchronously, whatever the format. Used by launch
    /// and by the running-app `OpenFile`.
    ///
    /// Mirrors the cache-miss navigation flow (`after_position_change`): set `pending_current`,
    /// size the window from ImageIO dims (metadata only, no decode), and let the preloader's
    /// prioritized target ship a `Preview` (RAW only) then a `Ready` that `poll_preloader`
    /// displays. Nothing blocks the main thread, so the window is up and painting while the file
    /// is still being read — which is the whole point on a share where the read takes seconds.
    ///
    /// The title stays the plain filename: the folder hasn't been scanned yet, so there's no
    /// `n/N` to show and no honest "Loading…" count either. `install_scanned_folder` fills the
    /// position in when the scan lands. The centered "Loading…" overlay waits out
    /// `navigation::LOADING_OVERLAY_DELAY`, so a local file never flashes it.
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

    /// Warm the browse selection's prospective current image + its neighbors into the image cache,
    /// so that when the user reveals it (Esc / Enter / double-click) the image shows instantly and
    /// arrowing left/right in image mode afterward is warm. Called when the browse selection lands
    /// on an image (a grid click, or the seeded selection after a folder lists). The browse
    /// selection IS the prospective current image; `reveal_selected_image` makes it current at
    /// reveal time. Warming runs BY PATH via `Preloader::warm_paths` (the cache is path-keyed, so
    /// warming arbitrary paths is safe) and deliberately does NOT display the image or auto-fit the
    /// window while browsing — doing so would resize the window behind the browse UI. A moved
    /// selection cancels the now-stale warms (`warm_paths` cancels paths that drop out of the new
    /// set). No-op where there's no browser, when neighbor preloading is disabled, or with no
    /// selection.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn warm_browse_selection(&mut self) {
        if !self.navigation.preload_neighbors {
            return;
        }
        let Some((images, selected)) = self.browser.grid_warm_target() else {
            return;
        };
        let total = images.len();
        let tasks: Vec<(usize, PathBuf)> =
            crate::browser::browse_warm_indices(selected, total, preloader::preload_count())
                .into_iter()
                .filter_map(|i| images.get(i).map(|p| (i, p.clone())))
                .collect();
        if tasks.is_empty() {
            return;
        }
        if let Some(preloader) = &mut self.navigation.preloader {
            preloader.warm_paths(tasks, total);
        }
    }

    /// Re-anchor browse mode to the live current image: reveal + select that image's folder in the
    /// tree (async expand-walk) and preselect the image in the grid, scrolling both into view. Runs
    /// on **every** entry into browse (right after `enter_browse`), not just first entry — so
    /// re-entering browse after navigating in image mode always shows the image you're currently
    /// viewing, never the stale selection from the last time you browsed. The current image is
    /// `dir_list`'s current entry; the anchor target (its folder + the image) is computed by the
    /// pure `browser::browse_anchor_target`. The reveal's tree selection lists the folder, and the
    /// stored preselect then anchors + scrolls the grid to the came-from image — so Esc/Enter right
    /// after open reveals the same image. When the folder is already the selected one,
    /// `select_and_scroll_to` still drives a re-list so the grid re-anchors (see its docs). No
    /// current image (nothing opened) → nothing to reveal; browse falls back to the last folder /
    /// home. No-op where there's no browser.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn reveal_current_image_in_browse(&mut self) {
        let current = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.current().to_path_buf());
        let Some((folder, image)) = crate::browser::browse_anchor_target(current.as_deref()) else {
            return;
        };
        self.browser.reveal_to_folder(&folder, Some(image));
    }

    pub(crate) fn display_current_async(&mut self, path: &Path) {
        let Some((index, total)) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| (d.current_index(), d.len()))
        else {
            // No directory (shouldn't happen — every call site sets one up first), but keep the
            // app working with a direct synchronous decode.
            self.display_image(path);
            return;
        };

        log::info!("Opening {} asynchronously", path.display());
        // Seed the preview scheduler with the list we have, BEFORE the auto-fit below: that reads
        // source dimensions through `previews`, and a stale folder there would size the window from
        // some other image. `install_scanned_folder` re-seeds with the full folder later.
        self.seed_previews_with_current_folder();

        self.navigation.pending_current = Some(index);
        self.request_times.insert(index, Instant::now());

        // Size the window from the RAW's header geometry (no develop) so it's
        // correct before any pixels paint. `previews::metadata` reads it with
        // the same crate that develops the file, so the number matches what
        // lands and the window never resizes twice.
        self.on_primary_decode_started();
        self.apply_preview_auto_fit(index);

        // Filename only while the folder is unscanned (the provisional list is 1 long, so
        // `window_title_with_position` drops the `n/N`); the real position arrives with the scan.
        if let Some(win) = &self.window {
            window::set_title_keeping_buttons(
                win,
                &window::window_title_with_position(path, index, total),
            );
        }

        if let Some(preloader) = &mut self.navigation.preloader {
            preloader.prioritize_target(index, path.to_path_buf(), total);
        }

        // The window/surface exist, but nothing has painted yet. Request a redraw so the empty
        // canvas (and, past the delay, the "Loading…" overlay) shows before the first
        // `Preview`/`Ready` arrives.
        self.request_redraw();
    }

    /// Apply a finished folder scan to image mode. Two shapes, per `navigation::ScanLanding`.
    ///
    /// **Keep the open image.** The provisional single-image list is swapped for the real folder
    /// and the picture on screen stays where it is: `DirectoryList::from_scan` finds it by path and
    /// positions there. Everything that needs the whole folder then catches up — the title's `n/N`,
    /// the preview scheduler's folder, the preloader's neighbor window. If the image isn't in the
    /// listing (deleted while the scan ran), the provisional list stays and so does the picture.
    ///
    /// **Play from the top.** Nothing was opened out of the folder: it was named on the command
    /// line, or dropped on a platform with no browser. Its images become the list and the first one
    /// is displayed. A folder with no images leaves whatever is on screen alone, and puts up the
    /// "(No images)" empty state when that's nothing.
    ///
    /// `scan_pending` clears either way — there's no second scan coming, and leaving it set would
    /// freeze navigation for good.
    fn install_scanned_folder(&mut self, folder: &Path, images: Vec<PathBuf>) {
        let Some(pending) = self.navigation.scan_pending.take() else {
            return;
        };
        match pending.landing {
            navigation::ScanLanding::KeepOpenImage => self.keep_open_image(folder, images),
            navigation::ScanLanding::PlayFromTop => self.play_scanned_folder(folder, images),
        }
    }

    /// The `ScanLanding::KeepOpenImage` half of [`install_scanned_folder`].
    fn keep_open_image(&mut self, folder: &Path, images: Vec<PathBuf>) {
        let Some((current_path, sort_by)) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| (d.current().to_path_buf(), d.sort_by()))
        else {
            return;
        };

        let image_count = images.len();
        let Some(list) = directory::DirectoryList::from_scan(images, sort_by, &current_path) else {
            log::warn!(
                "Scanned {} ({image_count} image(s)) but {} isn't in it — staying on it alone",
                folder.display(),
                current_path.display()
            );
            return;
        };
        let index = list.current_index();
        let total = list.len();
        log::info!(
            "Folder scan landed: {} image(s) in {}, showing {}/{total}",
            total,
            folder.display(),
            index + 1
        );
        self.navigation.dir_list = Some(list);

        // The window title gained its position; a still-decoding image keeps its "Loading…" title.
        if let Some(win) = &self.window {
            let title = if self.navigation.pending_current.is_some() {
                window::window_title_loading(index, total)
            } else {
                window::window_title_with_position(&current_path, index, total)
            };
            window::set_title_keeping_buttons(win, &title);
        }

        // The preloader was given the provisional list's slot 0; the image is the same, so the
        // in-flight decode still lands (`poll_preloader` matches the pending target by path), but
        // `pending_current` has to follow the image to its real index.
        if let Some(previous_slot) = self.navigation.pending_current.replace(index) {
            // The "displayed after Xms" clock is keyed by slot; move it with the image.
            if let Some(requested_at) = self.request_times.remove(&previous_slot) {
                self.request_times.insert(index, requested_at);
            }
        } else {
            self.navigation.pending_current = None;
        }

        self.seed_previews_with_current_folder();
        self.warm_initial_neighbors();
        self.request_redraw();
    }

    /// The `ScanLanding::PlayFromTop` half of [`install_scanned_folder`].
    fn play_scanned_folder(&mut self, folder: &Path, images: Vec<PathBuf>) {
        if images.is_empty() {
            log::info!(
                "Folder scan landed: nothing to show in {}",
                folder.display()
            );
            // A folder dropped over a picture leaves the picture; a folder argument had nothing
            // to lose, so it gets the empty state and Cmd/Ctrl+O as the way out.
            if self.navigation.dir_list.is_none() {
                self.empty_state = Some(EmptyState::NoImages);
                self.request_redraw();
            }
            return;
        }
        let sort_by = self
            .navigation
            .dir_list
            .as_ref()
            .map(directory::DirectoryList::sort_by)
            .unwrap_or_default();
        let list = directory::DirectoryList::from_explicit(images, sort_by, None);
        let first = list.current().to_path_buf();
        log::info!(
            "Folder scan landed: {} image(s) in {}, playing from the top",
            list.len(),
            folder.display()
        );
        self.file_path = first.clone();
        self.navigation.dir_list = Some(list);
        self.empty_state = None;
        self.seed_previews_with_current_folder();
        // Live folder sync: the folder being played is now the active one.
        self.retarget_active_folder_watch();
        self.display_current_async(&first);
        self.warm_initial_neighbors();
        self.request_redraw();
    }

    /// Hand the preview state the folder the navigation list now holds, positioned at the current
    /// image. The dimension prefetcher warms `(width, height)` around it on every platform, and on
    /// macOS the scheduler queues a QuickLook preview per index in priority order.
    fn seed_previews_with_current_folder(&mut self) {
        let Some((paths, index)) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|dir| (dir.files(), dir.current_index()))
        else {
            return;
        };
        self.previews.set_folder(paths, index);
        #[cfg(target_os = "macos")]
        self.pump_preview_requests();
    }

    /// The active folder whose images are on screen: in **browse mode** the grid's listed folder
    /// (what the user is looking at), in **image mode** the current image's parent. They coincide
    /// once synced, but a user can browse a different folder than the open image — we watch what's
    /// shown. `None` when nothing's listed/open (empty state). Drives `retarget_active_folder_watch`.
    fn active_folder(&self) -> Option<PathBuf> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if self.browser.is_browse() {
            return self.browser.selected_folder().map(Path::to_path_buf);
        }
        self.navigation
            .dir_list
            .as_ref()
            .map(|d| d.current())
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
    }

    /// Point the live folder-sync watcher at the active folder, unwatching the previously watched
    /// one. Call after the active folder changes — open a file/dir, reveal from browse, select a
    /// folder in the browse tree, re-scan that empties the folder, switch modes. The active folder
    /// follows the grid's listed folder in browse and the current image's folder in image mode
    /// (`active_folder`). No active folder (empty state) leaves nothing watched. No-op when the
    /// watcher couldn't start. This is the **image-list watch**, distinct from the tree-structure
    /// watch (`watch_tree_folder`/`unwatch_tree_folder`); a single folder can be both.
    pub(crate) fn retarget_active_folder_watch(&mut self) {
        let new_folder = self.active_folder();

        if new_folder == self.watched_folder {
            return;
        }

        // Don't unwatch the old active folder if it's still a watched tree node (a folder can be
        // both the listed folder and an expanded tree node / root). Only the role that owned it for
        // the image-list watch is ending; the tree watch must persist.
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        let old_still_tree_watched = self.watched_folder.as_ref().is_some_and(|old| {
            self.watched_tree_folders
                .iter()
                .any(|p| crate::paths::same_path(p, old))
        });
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let old_still_tree_watched = false;

        if let Some(watcher) = &self.folder_watcher {
            if let Some(old) = self.watched_folder.take()
                && !old_still_tree_watched
            {
                watcher.unwatch(old);
            }
            if let Some(new) = &new_folder {
                watcher.watch(new.clone());
            }
        }
        self.watched_folder = new_folder;
    }

    /// Watch the browse-tree roots for subdirectory changes (live folder sync, Part B). Roots stay
    /// watched for the window's life (they never collapse out of watching), so this is called once,
    /// when the browser is first built (browse entry / dir-arg launch). Idempotent: a root
    /// already in the set is skipped. No-op where there's no browser or before the tree exists.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(crate) fn watch_tree_roots(&mut self) {
        for root in self.browser.tree_root_paths() {
            self.watch_tree_folder(root);
        }
    }

    /// Start watching a tree folder for subdirectory changes and record it in the tree-watch set
    /// (live folder sync, Part B). Called on a node expand and for each root at setup. Idempotent —
    /// a folder already watched is skipped (so a root re-added at a later browse entry is harmless,
    /// and `notify` re-watch is a no-op anyway). No-op off the watcher.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(crate) fn watch_tree_folder(&mut self, folder: PathBuf) {
        if self.watched_tree_folders.contains(&folder) {
            return;
        }
        if let Some(watcher) = &self.folder_watcher {
            watcher.watch(folder.clone());
        }
        log::debug!("Tree-watch added: {}", folder.display());
        self.watched_tree_folders.push(folder);
    }

    /// Stop watching a tree folder on collapse (live folder sync, Part B), unless it's a **root**
    /// (roots stay watched for the window's life) or it's still the **active image-list folder**
    /// (that watch is owned separately by `watched_folder` — don't pull it out from under the grid
    /// / image sequence). Removes it from the tree-watch set. No-op off the watcher.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(crate) fn unwatch_tree_folder(&mut self, folder: &Path) {
        let Some(pos) = self
            .watched_tree_folders
            .iter()
            .position(|p| crate::paths::same_path(p, folder))
        else {
            return;
        };
        // Roots are never unwatched.
        if self
            .browser
            .tree_root_paths()
            .iter()
            .any(|r| crate::paths::same_path(r, folder))
        {
            return;
        }
        self.watched_tree_folders.remove(pos);
        // Keep the watch alive if it's still the active image-list folder (a folder can be both a
        // collapsed tree node and the grid's/image's folder).
        if self
            .watched_folder
            .as_deref()
            .is_some_and(|watched| crate::paths::same_path(watched, folder))
        {
            log::debug!(
                "Tree-watch removed but kept (still active folder): {}",
                folder.display()
            );
            return;
        }
        if let Some(watcher) = &self.folder_watcher {
            watcher.unwatch(folder.to_path_buf());
        }
        log::debug!("Tree-watch removed: {}", folder.display());
    }

    /// Ask the shared scanner to read `folder`. Fire-and-forget: the result arrives as
    /// `AppCommand::FolderScanned` and `handle_folder_scanned` hands it to whoever is waiting.
    /// Requests for a folder already queued or running are deduped inside the scanner.
    pub(crate) fn request_folder_scan(&self, folder: PathBuf) {
        let Some(scanner) = &self.folder_scanner else {
            log::debug!("Folder scanner isn't up yet — dropping scan request");
            return;
        };
        scanner.request(folder);
    }

    /// Ask the shared scanner to read `folder`, naming the one child a reveal walk needs listed
    /// however hidden it is (`folder_scan::FolderScanner::request_revealing`). `None` is an
    /// ordinary scan.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(crate) fn request_folder_scan_revealing(
        &self,
        folder: PathBuf,
        reveal_child: Option<PathBuf>,
    ) {
        match reveal_child {
            Some(child) => {
                let Some(scanner) = &self.folder_scanner else {
                    log::debug!("Folder scanner isn't up yet — dropping scan request");
                    return;
                };
                scanner.request_revealing(folder, child);
            }
            None => self.request_folder_scan(folder),
        }
    }

    /// Route one finished folder scan to every consumer waiting on that folder. A single scan can
    /// serve several at once: the tree row for a folder image mode just opened, say, or the grid
    /// and the image sequence when they're showing the same folder.
    pub(crate) fn handle_folder_scanned(
        &mut self,
        folder: PathBuf,
        images: Vec<PathBuf>,
        subdirs: Vec<PathBuf>,
    ) {
        // ── Browse tree: subdirectory rows. Ignores folders it has no scan in flight for. ──
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        self.browser.tree_children_loaded(&folder, subdirs);
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = subdirs;

        // ── Image mode: swap the provisional single-file list for the real folder. ──
        if self
            .navigation
            .scan_folder()
            .is_some_and(|pending| crate::paths::same_path(pending, &folder))
        {
            self.install_scanned_folder(&folder, images.clone());
        }

        // ── Browse grid: the folder the user selected in the tree. ──
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if self
            .pending_grid_listing
            .as_deref()
            .is_some_and(|pending| crate::paths::same_path(pending, &folder))
        {
            self.pending_grid_listing = None;
            log::info!(
                "Browse: folder listed {} ({} image(s))",
                folder.display(),
                images.len()
            );
            if let Some(win) = self.window.clone() {
                self.browser
                    .grid_folder_listed(&folder, images.clone(), &win);
            }
            // The listing seeds a selection (index 0); warm it + neighbors so the likely-opened
            // image is ready. Doesn't touch the displayed image.
            self.warm_browse_selection();
            // Live folder sync: in browse the active (image-list) watch follows the grid's listed
            // folder, so re-target it onto the folder that just listed.
            self.retarget_active_folder_watch();
        }

        // ── Live folder sync: diff the fresh listing against what's on screen. ──
        if self
            .pending_rescan
            .as_deref()
            .is_some_and(|pending| crate::paths::same_path(pending, &folder))
        {
            self.pending_rescan = None;
            self.apply_folder_rescan(&folder, images);
        }

        self.update_shared_state();
    }

    /// Handle a coalesced filesystem change for `folder`. When `folder` is the active (watched)
    /// folder, re-scan it OFF the main thread (a slow SMB folder must never block here — the shared
    /// `folder_scan::FolderScanner` does every directory read) and finish in `apply_folder_rescan`
    /// once the scan posts back as `FolderScanned`. `modified` paths are evicted from the image and
    /// preview caches right away (cheap, no I/O) so nothing stale is served; a modified
    /// currently-displayed image is re-decoded after the re-scan lands.
    ///
    /// **Routing.** One `FolderChanged` can match two roles, and BOTH fire:
    /// - the **active (image-list) folder** (the grid's listed folder in browse, the current
    ///   image's folder in image mode) → evict caches + re-scan off-thread (`apply_folder_rescan`
    ///   updates the grid and/or `dir_list`);
    /// - a **watched tree folder** (a currently-expanded tree node) → re-scan its subdirectories so
    ///   the tree reloads (`reload_tree_node`). A folder can be both the listed folder and an
    ///   expanded node, so neither branch is `else` to the other.
    ///
    /// A change matching neither role is ignored (we only watch what's on screen).
    pub(crate) fn handle_folder_changed(&mut self, folder: &Path, modified: &[PathBuf]) {
        let is_active = self
            .watched_folder
            .as_deref()
            .is_some_and(|watched| crate::paths::same_path(watched, folder));

        // ── Tree-structure watch: an expanded tree node changed → reload its subdirectories. ──
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if self
            .watched_tree_folders
            .iter()
            .any(|p| crate::paths::same_path(p, folder))
        {
            log::debug!(
                "Watched tree folder changed: {} — re-scanning subdirs",
                folder.display()
            );
            self.browser.reload_tree_node(folder);
        }

        if !is_active {
            return;
        }
        log::debug!(
            "Active folder changed: {} ({} modified) — re-scanning off-thread",
            folder.display(),
            modified.len()
        );

        // Evict modified paths from the image cache so a re-decode picks up fresh bytes. Cheap,
        // no I/O — safe inline.
        for path in modified {
            self.navigation.image_cache.remove(path);
        }
        for path in modified {
            self.previews.forget_path(path);
        }

        // Stash the modified set so `apply_folder_rescan` knows which paths to re-decode/repaint
        // once the off-thread listing returns.
        self.pending_modified = modified.to_vec();

        // Re-scan off the main thread; the result arrives as `AppCommand::FolderScanned`, where
        // `apply_folder_rescan` diffs it against the live list. The scanner is the app's one
        // directory-reading worker — nothing reads a folder on the main thread, because a slow SMB
        // folder would freeze the UI.
        self.pending_rescan = Some(folder.to_path_buf());
        self.request_folder_scan(folder.to_path_buf());
    }

    /// Apply a completed off-thread re-scan to the live `DirectoryList`. Diffs the fresh list
    /// against the current one (pure `folder_diff`), then:
    /// - inserts adds / drops removes at the right sorted positions,
    /// - keeps the current image pointed-to by path (existing re-sort-by-path behavior),
    /// - on a deleted current, navigates to the next image (or previous if last), or enters the
    ///   "(No images)" empty state if the folder is now imageless,
    /// - re-decodes + repaints a modified currently-displayed image (from `pending_modified`).
    ///
    /// Called from `handle_folder_scanned` when a re-scan (not a browse folder selection) is
    /// outstanding. Never blocks: the only decode here is the
    /// currently-displayed image via the normal display path (cache hit after eviction → async
    /// re-decode), matching the spec's "re-decode + repaint seamlessly".
    ///
    /// **Both modes update from one re-scan.** When the changed folder is the browse grid's listed
    /// folder, the grid is updated (insert adds at the sorted position, drop removes, keep the
    /// selection by path, refresh thumbnails). When it's the current image's folder, the image-mode
    /// `dir_list` is updated as below. They coincide when synced, so both fire and stay coherent.
    pub(crate) fn apply_folder_rescan(&mut self, folder: &Path, images: Vec<PathBuf>) {
        // Ignore a stale re-scan for a folder we've since navigated away from.
        if !self
            .watched_folder
            .as_deref()
            .is_some_and(|watched| crate::paths::same_path(watched, folder))
        {
            self.pending_modified.clear();
            return;
        }

        // ── Browse grid update (when this is the grid's listed folder) ──
        // Update the grid first, off the same re-scan. The grid preserves its selection by path,
        // inserts/removes at the sorted position, and refreshes thumbnails for the change. A
        // changed selection re-warms the prospective current image.
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let is_grid_folder = self
                .browser
                .selected_folder()
                .is_some_and(|selected| crate::paths::same_path(selected, folder));
            if is_grid_folder && let Some(win) = self.window.clone() {
                let modified = self.pending_modified.clone();
                let selection_changed =
                    self.browser
                        .apply_grid_rescan(images.clone(), &modified, &win);
                if selection_changed {
                    self.warm_browse_selection();
                }
            }
        }

        let sort_by = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.sort_by())
            .unwrap_or_default();
        let old: Vec<PathBuf> = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.files_ref().to_vec())
            .unwrap_or_default();
        let current = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.current().to_path_buf());

        // Only drive the image-mode sequence when this folder is the current image's folder. In
        // browse mode the grid's folder may differ from the open image's folder (the user browsed
        // elsewhere); then the grid updated above and `dir_list` must stay put.
        let is_image_folder = current
            .as_deref()
            .and_then(Path::parent)
            .map(|p| crate::paths::same_path(p, folder))
            .unwrap_or(false)
            // In the image-mode empty state there's no current image, but the watched folder IS
            // the (emptied) image folder — let a re-appearing image recover it.
            || (current.is_none() && !self.browser.is_browse());
        if !is_image_folder {
            self.pending_modified.clear();
            self.update_shared_state();
            return;
        }

        let modified = std::mem::take(&mut self.pending_modified);
        let current_path_modified = current
            .as_ref()
            .is_some_and(|c| modified.iter().any(|m| m == c));

        let diff =
            crate::navigation::folder_diff::diff_folder(&old, images, sort_by, current.as_deref());

        use crate::navigation::folder_diff::CurrentOutcome;
        match diff.current {
            CurrentOutcome::Empty => {
                log::info!("Active folder emptied — entering image-mode (No images) state");
                self.enter_no_images_state();
                self.update_shared_state();
            }
            CurrentOutcome::Unchanged { index } => {
                let list_changed = !diff.list_unchanged();
                self.navigation.dir_list =
                    Some(crate::navigation::directory::DirectoryList::from_sorted(
                        diff.sorted,
                        sort_by,
                        index,
                    ));
                self.empty_state = None;
                // The index map shifted (adds/removes around the current image), so re-seed the
                // preview folder + refresh the preload window against the new list.
                self.reseed_after_rescan(index);
                if current_path_modified {
                    // The displayed image's bytes changed — re-decode + repaint via the normal
                    // path. The cache entry was evicted above, so this re-reads from disk.
                    self.refresh_current_after_modify(index);
                } else if list_changed {
                    self.request_redraw();
                }
                self.update_shared_state();
            }
            CurrentOutcome::Navigate { index } => {
                // Current image was deleted → land on the chosen successor/previous via the normal
                // display path (instant from cache, else async decode with a placeholder).
                self.navigation.dir_list =
                    Some(crate::navigation::directory::DirectoryList::from_sorted(
                        diff.sorted,
                        sort_by,
                        index,
                    ));
                self.empty_state = None;
                self.navigation.pending_current = None;
                self.navigation.last_direction = crate::navigation::directory::Direction::Unknown;
                self.reseed_after_rescan(index);
                self.display_after_delete(index);
                self.update_shared_state();
            }
        }
    }

    /// Re-seed the preview scheduler with the post-rescan folder + refresh the preloader's active
    /// window. Shared by the add/remove and delete-current paths.
    fn reseed_after_rescan(&mut self, current_index: usize) {
        if let Some(dir) = &self.navigation.dir_list {
            let paths = dir.files();
            self.previews.set_folder(paths, current_index);
        }
        self.refresh_preload_window();
    }

    /// Re-decode + repaint the currently-displayed image after its file changed on disk. The cache
    /// entry was already evicted, so this goes through the normal display path: a cache miss shows
    /// the (now-stale-evicted) preview placeholder briefly, the async decode swaps in the fresh
    /// pixels. Mirrors `display_open_target` for the current index.
    fn refresh_current_after_modify(&mut self, index: usize) {
        let Some((path, total)) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| (d.current().to_path_buf(), d.len()))
        else {
            return;
        };
        log::info!(
            "Current image modified on disk — re-decoding {}",
            path.display()
        );
        self.navigation.pending_current = Some(index);
        self.request_times.insert(index, Instant::now());
        if let Some(win) = &self.window {
            window::set_title_keeping_buttons(win, &window::window_title_loading(index, total));
        }
        if let Some(preloader) = &mut self.navigation.preloader {
            preloader.prioritize_target(index, path, total);
        }
        self.request_redraw();
    }

    /// Display the image at `index` after the current one was deleted. Instant from cache, else the
    /// async placeholder path — never blocks the main thread. Mirrors the cache-hit/miss branches
    /// of `display_open_target` without the browse-specific bits.
    fn display_after_delete(&mut self, index: usize) {
        let Some((path, total)) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| (d.current().to_path_buf(), d.len()))
        else {
            return;
        };
        self.request_times.insert(index, Instant::now());
        if self.navigation.image_cache.contains(&path) {
            self.navigation.pending_current = None;
            if let Some(win) = &self.window {
                window::set_title_keeping_buttons(
                    win,
                    &window::window_title_with_position(&path, index, total),
                );
            }
            self.display_from_cache(index);
        } else {
            self.navigation.pending_current = Some(index);
            if let Some(win) = &self.window {
                window::set_title_keeping_buttons(win, &window::window_title_loading(index, total));
            }
            if !self.display_preview_placeholder(index) {
                self.apply_preview_auto_fit(index);
                if let Some(renderer) = &mut self.renderer {
                    renderer.clear_image();
                }
            }
            self.request_redraw();
            if let Some(preloader) = &mut self.navigation.preloader {
                preloader.prioritize_target(index, path, total);
            }
        }
    }

    /// Enter the image-mode "(No images)" empty state: clear the bound image (so the canvas fills
    /// with opaque black), drop the directory list (nothing to navigate), and flag the empty state
    /// so `render_frame` draws the centered "(No images)" overlay. The watch stays on the folder so
    /// a newly-added image reappears.
    fn enter_no_images_state(&mut self) {
        self.navigation.dir_list = None;
        self.navigation.pending_current = None;
        self.navigation.current_image_size = None;
        self.empty_state = Some(EmptyState::NoImages);
        if let Some(renderer) = &mut self.renderer {
            renderer.clear_image();
        }
        if let Some(win) = &self.window {
            window::set_title_keeping_buttons(win, "Prvw");
        }
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
                crate::diagnostics::format_bytes(e.memory_cost),
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
        if self.navigation_blocked_by_scan() {
            return;
        }
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
        if delta == 0 || self.navigation_blocked_by_scan() {
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
        if self.navigation_blocked_by_scan() {
            return;
        }
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
        if self.navigation_blocked_by_scan() {
            return;
        }
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

    /// Start or stop the slideshow (Slideshow → Start/Stop, or bare `S`).
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
            menu.set_slideshow_running(self.slideshow.running);
        }
    }

    // ── Browse mode ──────────────────────────────────────────────────

    /// Reveal the browse-selected image in image mode. **Esc and Enter both route here** (the
    /// user's model: the image-mode current image IS whatever the browse cursor points at, even
    /// while the Metal canvas is hidden) — there's no "Esc preserves the old image" path anymore.
    ///
    /// The reveal is **black-not-stale**, never the previous image. The Metal layer's last-visible
    /// frame was made black on browse entry (`set_view_mode` clears the image + paints once while
    /// visible), because presenting to a hidden layer doesn't commit. So we unhide the canvas
    /// ([`browser::State::reveal_image_canvas`]), point `navigation` at the grid's folder + selected
    /// index, then synchronously paint that image (cache hit → correct image in one frame; miss →
    /// the grid thumbnail's correct-aspect QuickLook preview + "Loading…", or clean black if no
    /// preview is cached, with the sharp decode swapping in later via `poll_preloader`). The worst
    /// the user can see is a brief black → correct image, never the stale stretched previous image.
    ///
    /// **No selection** (empty folder, or the tree is focused with no grid pick): degrade
    /// gracefully — reveal image mode still showing the last valid image (whatever `dir_list`
    /// currently holds), or a clean empty canvas if nothing was ever opened. Never a blank/stale
    /// flash, never a crash. No-op where there's no browser, or when already in image mode.
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))] // only the browse exits call this
    pub(crate) fn reveal_selected_image(&mut self) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            if !self.browser.is_browse() {
                return;
            }

            // The browse selection drives the current image: only a focused grid with a live
            // selection picks a specific image. Esc on the tree (or with no selection) keeps the
            // currently displayed image — degrade gracefully, don't blank the canvas.
            let grid_focused = matches!(
                self.browser.focused_pane(),
                Some(crate::browser::PaneSide::Grid)
            );
            let target = if grid_focused {
                self.browser.grid_open_target()
            } else {
                None
            };

            if let Some((path, images, index)) = target {
                log::info!(
                    "Browse reveal: {} (index {index} of {})",
                    path.display(),
                    images.len()
                );
                // Point navigation at the grid's folder, positioned at the selected index. The
                // grid lists in the same `SortBy`, so `from_explicit` (which re-sorts with the
                // same comparator) maps the grid's selected index 1:1 to the dir-list index
                // (`resolve_reveal_index`), landing `go_by` exactly on `path`.
                let sort_by = settings::Settings::load().sort_by;
                let resolved =
                    crate::browser::resolve_reveal_index(&images, index, sort_by).unwrap_or(0);
                let mut dir_list = directory::DirectoryList::from_explicit(images, sort_by, None);
                if resolved > 0 {
                    dir_list.go_by(resolved as i32, false);
                }
                self.navigation.dir_list = Some(dir_list);

                // Seed the preview scheduler with the new folder (placeholder support + prefetch).
                if let Some(dir) = &self.navigation.dir_list {
                    let paths = dir.files();
                    let current = dir.current_index();
                    self.previews.set_folder(paths, current);
                }
            } else {
                log::info!("Browse reveal: no grid selection — keeping the current image");
            }

            // ── Unhide, then render (black-not-stale) ──
            // Set image-mode state + hide the split view + unhide the Metal layer. Presenting to a
            // hidden `CAMetalLayer` doesn't commit, so painting while hidden then unhiding (the old
            // "render-then-unhide") didn't work — the layer kept its last-VISIBLE frame for ~100 ms.
            // Instead the layer's last-visible frame was made black on browse entry (`set_view_mode`
            // → `clear_image` + paint), so the worst case here is a brief black, never the stale
            // stretched previous image. We unhide first, then synchronously paint the target.
            let Some(win) = self.window.clone() else {
                return;
            };
            self.browser.reveal_image_canvas(&win);

            // Paint the selected (or current) image into the renderer — instant from cache, else a
            // correct-aspect placeholder (or clean black) while the background decode runs. NEVER
            // blocks the main thread on a full decode. With no image at all (nothing ever opened),
            // the renderer's black image-area fill keeps the canvas clean — never stale.
            if self.navigation.dir_list.is_some() {
                self.empty_state = None;
                self.display_open_target();
                // Warm neighbors so arrow-key nav is instant (cache-miss queues them after `Ready`;
                // this covers the cache-hit case where no `Ready` fires).
                if self.navigation.pending_current.is_none() {
                    self.warm_initial_neighbors();
                }
                // Live folder sync: watch the revealed image's folder (re-target off the old one).
                self.retarget_active_folder_watch();
            }

            // Re-assert the title/zoom labels against the title-bar / fullscreen state (browse hid
            // them) before the synchronous paint, so the first visible frame has the right chrome.
            // macOS only: Windows draws its title in the frame, so there is no strip to reserve
            // and `CommandKey::TitleBar` is `NotApplicable` there.
            #[cfg(target_os = "macos")]
            {
                let offset = self.content_offset_y();
                window::set_titlebar_vibrancy_visible(&win, offset.0 > 0.0);
            }

            // Paint the now-visible drawable: a cache hit lands the correct image in this one frame
            // (black → image), a miss lands a correct-aspect placeholder or clean black.
            self.render_frame();
            self.needs_redraw = false;

            self.set_browse_menu_label();
            // Keep render-on-demand honest: request a follow-up frame for the placeholder→sharp
            // swap and any auto-fit settling.
            self.request_redraw();
            self.update_shared_state();
        }
    }

    /// Display the current `dir_list` image when opening from the browse grid: instant from cache
    /// if present, else the async placeholder path (set `pending_current`, show the preview
    /// placeholder or a metadata-only auto-fit, "Loading…" title, and `prioritize_target` so the
    /// preloader decodes in the background). NEVER blocks the main thread on a full decode — that's
    /// the whole point of Fix #14. Mirrors the cache-hit / cache-miss branches of
    /// `after_position_change` (direction unknown). Browser-only, and the preview state it reads
    /// for the placeholder exists on both platforms that have one.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn display_open_target(&mut self) {
        let Some((path, index, total)) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| (d.current().to_path_buf(), d.current_index(), d.len()))
        else {
            return;
        };
        self.navigation.last_direction = directory::Direction::Unknown;
        self.request_times.insert(index, Instant::now());

        if self.navigation.image_cache.contains(&path) {
            // Warm hit — display instantly, no decode wait.
            self.navigation.pending_current = None;
            if let Some(win) = &self.window {
                window::set_title_keeping_buttons(
                    win,
                    &window::window_title_with_position(&path, index, total),
                );
            }
            self.display_from_cache(index);
            self.request_times.remove(&index);
            self.on_primary_decode_settled();
            self.on_preview_current_changed(index);
            log::info!(
                "Browse open: {} displayed from cache (instant)",
                path.display()
            );
        } else {
            // Cache miss — show a placeholder instantly and decode in the background. The full
            // image swaps in when `poll_preloader` sees `Ready` for `pending_current`.
            self.pending_crossfade = false;
            self.navigation.pending_current = Some(index);
            if let Some(win) = &self.window {
                window::set_title_keeping_buttons(win, &window::window_title_loading(index, total));
            }
            self.on_primary_decode_started();
            self.on_preview_current_changed(index);
            // The grid thumbnail's QuickLook preview is the same cache the image-mode placeholder
            // reads, so the correct-aspect preview shows at once; fall back to a metadata-only
            // auto-fit if no preview is cached yet. With NO placeholder we must drop any
            // still-bound image so the renderer fills the (newly auto-fit) image area with black
            // rather than stretching the previous image's texture to the new geometry — the
            // distorted stale-frame look. `clear_image` on browse entry already dropped it; this
            // is the belt-and-suspenders guarantee for any path that reaches here with a texture.
            if !self.display_preview_placeholder(index) {
                self.apply_preview_auto_fit(index);
                if let Some(renderer) = &mut self.renderer {
                    renderer.clear_image();
                }
            }
            self.needs_redraw = true;
            if let Some(preloader) = &mut self.navigation.preloader {
                preloader.prioritize_target(index, path, total);
            }
            log::info!("Browse open: cache miss — placeholder shown, decoding in background");
        }
    }

    pub(crate) fn set_view_mode(&mut self, target: crate::browser::ViewMode) {
        self.enter_view_mode(target, None);
    }

    /// Enter browse mode showing `folder`, whatever was on screen before. A dropped folder is
    /// what this is for: it names where to go, so the "reveal where you already are" step
    /// [`Self::set_view_mode`] runs would be a walk to the wrong place, and two reveals racing
    /// each other for the tree selection.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(crate) fn browse_folder(&mut self, folder: &Path) {
        self.enter_view_mode(crate::browser::ViewMode::Browse, Some(folder));
    }

    /// The mode switch itself. `reveal` names the folder browse should open at; `None` means
    /// "wherever the current image is", which is what every route but a dropped folder wants.
    fn enter_view_mode(&mut self, target: crate::browser::ViewMode, reveal: Option<&Path>) {
        if self.browser.mode() == target {
            // Already there, so there's no mode to switch — but a named folder still has to be
            // shown, or dropping one onto the browser would do nothing.
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            if let Some(folder) = reveal {
                self.browser.reveal_to_folder(folder, None);
                self.update_shared_state();
            }
            return;
        }
        // Only the branch below has a tree to reveal anything in.
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = reveal;

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        match self.window.clone() {
            // `enter_browse`/`enter_image` set `mode` + `focused_pane` then `sync_native` the result
            // (the single render-from-state choke-point) — no separate `toggle_mode` here.
            Some(win) => match target {
                crate::browser::ViewMode::Browse => {
                    // Make the Metal layer's LAST-VISIBLE composited frame black BEFORE browse
                    // hides it. Presenting to a hidden `CAMetalLayer` doesn't commit, so when a
                    // later reveal unhides it the layer shows whatever it last composited while
                    // visible. By clearing the bound image and painting one frame now (still
                    // visible), that last frame is opaque black — so the worst a reveal can show
                    // is black, never the stale stretched previous image. The split view covers
                    // the canvas immediately on `enter_browse`, so this black frame isn't seen.
                    //
                    // Windows doesn't need the trick — a swapchain isn't presented while the
                    // browser is up, so its last frame is simply whatever was there — but it
                    // costs one frame and it keeps the two platforms on one path.
                    if let Some(renderer) = &mut self.renderer {
                        renderer.clear_image();
                    }
                    self.render_frame();
                    self.browser.enter_browse(&win);
                    // Live folder sync (Part B): the browser (and its tree) are now built — watch
                    // the roots (idempotent, so re-entering browse is harmless).
                    self.watch_tree_roots();
                    // Browse-open positioning: reveal + select the folder in the tree (async
                    // walk) and preselect an image in the grid, so browse opens already showing
                    // where you are and Esc/Enter round-trips back to it. A dropped folder
                    // names its own target instead.
                    match reveal {
                        Some(folder) => self.browser.reveal_to_folder(folder, None),
                        None => self.reveal_current_image_in_browse(),
                    }
                }
                crate::browser::ViewMode::Image => self.browser.enter_image(&win),
            },
            // No window yet (defensive — browse is unreachable without one): still track the mode.
            None => {
                self.browser.toggle_mode();
            }
        }
        // With no native browser there's nothing to show; just track the mode so the rest of the
        // app agrees.
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        self.browser.toggle_mode();

        self.set_browse_menu_label();

        // Image mode needs a frame; browse mode goes idle (render-on-demand).
        if matches!(target, crate::browser::ViewMode::Image) {
            // Re-assert the title/zoom labels' visibility against the current title-bar / fullscreen
            // state (browse hid them via `sync_native`). The next redraw refreshes their text.
            #[cfg(target_os = "macos")]
            if let Some(win) = &self.window {
                let offset = self.content_offset_y();
                window::set_titlebar_vibrancy_visible(win, offset.0 > 0.0);
            }
            self.request_redraw();
        }
        // Live folder sync: the active (image-list) watch follows the grid's folder in browse and
        // the current image's folder in image mode, so re-target on every mode switch. (When
        // entering browse, the reveal's folder listing also re-targets via `BrowseFolderListed`;
        // this covers leaving browse and the no-reveal cases.)
        self.retarget_active_folder_watch();
        self.update_shared_state();
    }

    /// Update the Navigate menu item's label to match the current mode: "Image browser" while
    /// viewing an image (the action takes you there), "Image view" while browsing.
    fn set_browse_menu_label(&self) {
        if let Some(menu) = &self.app_menu {
            menu.set_browse_mode(self.browser.is_browse());
        }
    }

    /// Push freshly saved settings onto the menu's checkmarks. Every command that writes a
    /// setting the menu mirrors ends with this.
    pub(crate) fn sync_menu_from_settings(&self, settings: &settings::Settings) {
        if let Some(menu) = &self.app_menu {
            menu.sync_from_settings(settings);
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
        // A slow image decode: wake when the "Loading…" overlay is due so it appears on time
        // even if nothing else is happening. Fast decodes land first and never reach it.
        if let Some(due) = self.loading_overlay_deadline() {
            candidates.push(due);
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
            self.on_primary_decode_settled();
            self.on_preview_current_changed(current_index);
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
            // reaches the right size before pixels arrive. Off macOS
            // there is no QuickLook preview to upload, so the fit is the
            // whole of it.
            if !self.display_preview_placeholder(current_index) {
                self.apply_preview_auto_fit(current_index);
            }
            self.needs_redraw = true;
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
        // The window is current ± `preload_count()` on both sides regardless of
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

    /// Paths currently inside the hot preload window (current ± `preload_count()`,
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

    /// Render one frame to the wgpu drawable, returning whether it actually drew (false when the
    /// renderer is absent or the surface wasn't ready). Builds the title labels + histogram/EXIF
    /// overlays then calls `renderer.render`. Normally driven by `WindowEvent::RedrawRequested`,
    /// but also called synchronously from the browse→image reveal path: it paints the selected
    /// image to the drawable BEFORE the Metal layer is unhidden, so the first visible frame is
    /// already correct (no ~100 ms stale-image flash). See `reveal_selected_image`.
    fn render_frame(&mut self) -> bool {
        let mut text_blocks = self.build_text_overlay();
        let offset = self.content_offset_y();

        // The empty state: a centered overlay on the clean black canvas (nothing is bound to
        // the renderer, so it fills the image area with opaque black). Built here, not in
        // `build_text_overlay`, because that early-returns when there's no current image —
        // exactly the empty-state case. Same glyphon pill styling as the "Loading…" overlay.
        if let Some(empty_state) = self.empty_state
            && let Some(rend) = &self.renderer
        {
            let logical_width = rend.logical_width();
            let logical_height = rend.logical_height();
            let line_height = 18.0_f32;
            let center_x = Logical(logical_width.0 / 2.0);
            let center_y = Logical((logical_height.0 - line_height) / 2.0);
            let mut empty = text::TextBlock::new(empty_state.overlay(), center_x, center_y);
            empty.font_size = 14.0;
            empty.line_height = line_height;
            empty = empty.bold().align_center().pill(
                [0.0, 0.0, 0.0, 0.55],
                Logical(11.0),
                Logical(5.0),
                Logical(7.0),
            );
            text_blocks.push(empty);
        }

        // Files are hovering: say the window takes them. Without this a drag over a viewer that
        // does accept drops looks exactly like one over a viewer that doesn't. Same pill as the
        // empty state, and it replaces that one rather than stacking on it.
        if self.files_hovering
            && let Some(rend) = &self.renderer
        {
            let line_height = 18.0_f32;
            let center_x = Logical(rend.logical_width().0 / 2.0);
            let center_y = Logical((rend.logical_height().0 - line_height) / 2.0);
            // Draft copy, for David to review.
            let mut hint = text::TextBlock::new("Drop to open", center_x, center_y);
            hint.font_size = 14.0;
            hint.line_height = line_height;
            hint = hint.bold().align_center().pill(
                [0.0, 0.0, 0.0, 0.55],
                Logical(11.0),
                Logical(5.0),
                Logical(7.0),
            );
            if self.empty_state.is_some() {
                text_blocks.pop();
            }
            text_blocks.push(hint);
        }

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
            let build =
                histogram::overlay::build(data, self.histogram.hover_bin, width, overlay_offset);
            standalone_pills.extend(build.pills);
            for tb in build.text_blocks {
                text_blocks.push(tb);
            }
            Some(build.draw_call)
        } else {
            None
        };

        // EXIF info overlay. Hidden when the user toggled it off, or while the current image
        // isn't decoded yet (a placeholder is on screen, or nothing is) — claiming "no Exif
        // data" before we've read the file would be a guess. A decoded image with no EXIF DOES
        // get the panel, saying so: see `current_exif_state`.
        if self.exif_overlay.visible
            && let Some(width) = logical_width
            && let Some(metadata) = self.current_exif_state()
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

        self.renderer.as_mut().is_some_and(|renderer| {
            renderer.render(&text_blocks, &standalone_pills, histogram_call, offset)
        })
    }

    /// What we know about the current image's EXIF, in three states. The cache is the source of
    /// truth: navigation always insert-then-display, so the current image is cache-resident by
    /// the time we render its pixels.
    ///
    /// - `None` — nothing is displayed, or the decode hasn't landed yet (a preview placeholder
    ///   is on screen). We don't know, so the panel stays away rather than reporting an absence
    ///   it hasn't checked.
    /// - `Some(None)` — the image is decoded and carries no EXIF (PNG, GIF, BMP, a plain WebP,
    ///   a JPEG with no APP1 segment).
    /// - `Some(Some(_))` — the metadata itself.
    pub(crate) fn current_exif_state(&self) -> Option<Option<&decoding::ExifMetadata>> {
        let dir = self.navigation.dir_list.as_ref()?;
        let entry = self.navigation.image_cache.peek(dir.current())?;
        Some(entry.exif.as_deref())
    }

    /// EXIF metadata of the currently displayed image, if any. Collapses the "not decoded yet"
    /// and "decoded, no EXIF" cases of [`Self::current_exif_state`] into one `None`.
    pub(crate) fn current_exif(&self) -> Option<&decoding::ExifMetadata> {
        self.current_exif_state().flatten()
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

        // Centered "Loading..." overlay for a target that's taking a while (see
        // `loading_overlay_due` — a fast decode never shows it).
        // Styled like the title pill but larger — system font, bigger
        // font size, bigger corner radius to match. The `align_center`
        // flag measures the actual shaped text width at prepare time and
        // repositions the block so the text is truly centered on `x`;
        // the pill follows the text.
        if self.loading_overlay_visible {
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
    /// Whether the image we're waiting on has been pending long enough to earn the centered
    /// "Loading…" overlay. A decode that finishes inside `navigation::LOADING_OVERLAY_DELAY` never
    /// shows it, so opening a local file doesn't flash a spinner at the user. A pending target with
    /// no recorded request time (shouldn't happen) shows the overlay rather than hiding forever.
    fn loading_overlay_due(&self) -> bool {
        let Some(pending) = self.navigation.pending_current else {
            return false;
        };
        self.request_times
            .get(&pending)
            .is_none_or(|requested_at| requested_at.elapsed() >= navigation::LOADING_OVERLAY_DELAY)
    }

    /// When the "Loading…" overlay is due to appear, if it isn't showing yet. `about_to_wait` feeds
    /// this to `schedule_wakeup` so the overlay appears on time even with nothing else going on.
    fn loading_overlay_deadline(&self) -> Option<Instant> {
        if self.loading_overlay_visible {
            return None;
        }
        let pending = self.navigation.pending_current?;
        Some(*self.request_times.get(&pending)? + navigation::LOADING_OVERLAY_DELAY)
    }

    /// True while the opened folder is still being scanned, which is when navigation has nowhere to
    /// go: `dir_list` holds only the image on screen. Logs the ignored move at debug level.
    ///
    // TODO(slow-share-launch part B): instead of dropping the move, record it as a navigation
    // intent (anchor + signed delta) here and resolve it in `install_scanned_folder` once the real
    // list is in, so an arrow pressed during a slow scan still lands where the user meant.
    fn navigation_blocked_by_scan(&self) -> bool {
        let Some(folder) = self.navigation.scan_folder() else {
            return false;
        };
        log::debug!("Navigation ignored — still scanning {}", folder.display());
        true
    }

    /// The directory slot we're waiting on, when `path` is the file that slot holds.
    ///
    /// Matched **by path**, not by the index the preload task was queued under: the folder scan
    /// landing mid-decode reorders the list beneath an in-flight target (`install_scanned_folder`
    /// moves the opened image from provisional slot 0 to its real position). The path a task was
    /// queued for never changes, so it's the reliable identity.
    fn pending_slot_for(&self, path: &Path) -> Option<usize> {
        let pending = self.navigation.pending_current?;
        let current = self.navigation.dir_list.as_ref()?.get(pending)?;
        (current == path).then_some(pending)
    }

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
                    path,
                    image,
                    decode_duration,
                    file_name,
                } => {
                    if let Some(p) = &mut self.navigation.preloader {
                        p.mark_complete(&path);
                    }
                    let pending_slot = self.pending_slot_for(&path);
                    let evicted =
                        self.navigation
                            .image_cache
                            .insert(path, image, decode_duration, file_name);
                    self.log_evictions(evicted, "LRU");
                    if pending_slot.is_none() {
                        neighbor_arrived = true;
                    }
                    if let Some(index) = pending_slot {
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
                        let had_placeholder = self.placeholder_active;
                        self.placeholder_active = false;
                        self.record_preview_event(
                            "primary-arrived",
                            format!("index={index} had_placeholder={had_placeholder}"),
                        );
                        self.on_primary_decode_settled();
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
                    if self.pending_slot_for(&path).is_some() {
                        self.navigation.pending_current = None;
                        log::error!(
                            "Failed to decode current image {}: {reason}",
                            path.display()
                        );
                        if let Some(win) = &self.window {
                            window::set_title_keeping_buttons(win, &format!("Prvw - {reason}"));
                        }
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
                preloader::PreloadResponse::Preview { path, image } => {
                    // RAW embedded-JPEG preview: show it as a soft placeholder
                    // only while we're still waiting on THIS target's full
                    // develop. If a newer nav moved on (index no longer
                    // pending), or the full image already landed, drop it.
                    // macOS-only display (see `display_raw_preview_placeholder`).
                    #[cfg(target_os = "macos")]
                    if let Some(slot) = self.pending_slot_for(&path) {
                        log::debug!(
                            "Showing RAW preview placeholder [{slot}] {}",
                            path.display()
                        );
                        self.display_raw_preview_placeholder(slot, image);
                    }
                    #[cfg(not(target_os = "macos"))]
                    let _ = (path, image);
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
        if let Some(renderer) = &mut self.renderer {
            renderer.set_display_icc(self.color.display_icc.clone());
        }

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

    /// How much headroom above SDR white the display can show right now. `1.0` (no headroom, so
    /// the RAW decoder stays on its 8-bit path) until the renderer exists to ask. See
    /// [`renderer::Renderer::display_hdr_headroom`] for what each platform means by the number.
    fn query_edr_headroom(&self) -> f32 {
        self.renderer
            .as_ref()
            .map_or(1.0, renderer::Renderer::display_hdr_headroom)
    }

    /// Re-read the display's headroom and, when it moved, re-tune everything downstream of it: the
    /// decoder's output depth, the cache's memory budget, and the cache's contents (every buffer
    /// in it was decoded for the old headroom).
    fn refresh_edr_headroom(&mut self) {
        let new_headroom = self.query_edr_headroom();
        if (new_headroom - self.edr_headroom).abs() <= 1e-3 {
            return;
        }
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

    /// Re-read the display profile when a move put the window on a different monitor, and do
    /// nothing at all when it didn't.
    ///
    /// `apply_icc_settings` is the expensive half: it flushes the decoded-image cache and
    /// re-decodes, so it must not run because someone nudged the window two pixels. It has its own
    /// byte-equality guard, but that guard costs a profile read, which is a file read here.
    fn refresh_display_profile_after_move(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let Some(monitor) = display_profile::current_monitor(window) else {
            return;
        };
        if !self.color.monitors.moved_to(monitor) {
            return;
        }
        log::debug!("The window is on a different display, so its colour profile is re-read");
        // The new display may have different headroom, too: a laptop panel with HDR switched on
        // and an external SDR monitor is an ordinary Windows desk.
        self.refresh_edr_headroom();
        self.apply_icc_settings();
        self.apply_edr_surface_state();
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
            display_profile::set_layer_edr_state(
                win,
                want_hdr,
                &self.color.display_icc,
                self.edr_headroom,
            );
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
    ///
    /// The two platforms reach this from opposite directions. macOS is told
    /// the window changed screens (`NSWindowDidChangeScreenNotification`).
    /// Windows gets here from `WM_DISPLAYCHANGE`, which is the display
    /// *configuration* changing: a monitor arriving or leaving, a resolution
    /// change, or a profile re-associated in place. Any of those can leave the
    /// same `HMONITOR` meaning something else, so what the tracker knows is
    /// worth nothing afterwards and gets dropped.
    fn handle_display_changed(&mut self) {
        log::debug!("Display changed, re-evaluating ICC + EDR");
        self.color.monitors.forget();
        self.refresh_edr_headroom();
        self.apply_icc_settings();
        // `apply_icc_settings` re-decodes, which goes through `display_image`
        // and thus `apply_edr_surface_state`. If nothing changed (same
        // display, same ICC), still confirm the surface state matches the
        // latest headroom.
        self.apply_edr_surface_state();
    }

    /// The operating system's own handle for the main viewer window: the AppKit
    /// `NSWindow.windowNumber`. Used by the debug-only `screenshot_window` MCP tool, which
    /// photographs the window as a person sees it (overlays, vibrancy, title bar, the lot).
    /// `None` while the window doesn't exist yet or the number is non-positive.
    #[cfg(all(debug_assertions, target_os = "macos"))]
    pub(crate) fn native_window_id(&self) -> Option<u64> {
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
            Some(number as u64)
        }
    }

    /// The `HWND` of the main viewer window, as a `u64`. A number rather than an `HWND` because
    /// it crosses to worker threads, and `HWND` is a raw pointer and so not `Send`. `None` while
    /// the window doesn't exist yet.
    #[cfg(target_os = "windows")]
    pub(crate) fn main_window_hwnd(&self) -> Option<u64> {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let win = self.window.as_ref()?;
        let RawWindowHandle::Win32(handle) = win.window_handle().ok()?.as_raw() else {
            return None;
        };
        Some(handle.hwnd.get() as u64)
    }

    /// The Windows half of [`Self::native_window_id`], for the debug-only `screenshot_window`
    /// MCP tool. The handle itself is [`Self::main_window_hwnd`], which printing also needs.
    #[cfg(all(debug_assertions, target_os = "windows"))]
    pub(crate) fn native_window_id(&self) -> Option<u64> {
        self.main_window_hwnd()
    }

    /// Pop up the right-click context menu at the cursor. No-op when no image is open. The
    /// selected item posts a `MenuEvent` picked up by `handle_menu_event` on the next
    /// `about_to_wait`, same path as the menu bar.
    ///
    /// Both platforms with a menu bar have it; what the menu holds is the registry's answer
    /// (`parity::menu_items`), which today is Copy on Windows and Copy plus Print on macOS.
    /// Linux has no menu of any kind, so a right-click there does nothing.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn show_image_context_menu(&self) {
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
        let Ok(raw) = win.window_handle().map(|h| h.as_raw()) else {
            return;
        };
        #[cfg(target_os = "macos")]
        {
            let RawWindowHandle::AppKit(handle) = raw else {
                return;
            };
            let ns_view = handle.ns_view.as_ptr() as *const std::ffi::c_void;
            // SAFETY: winit gives us a valid `NSView*` for the main window.
            unsafe { app_menu.show_image_context_menu(ns_view) };
        }
        #[cfg(target_os = "windows")]
        {
            let RawWindowHandle::Win32(handle) = raw else {
                return;
            };
            // SAFETY: winit gives us a valid `HWND` for the main window.
            unsafe { app_menu.show_image_context_menu(handle.hwnd.get()) };
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

        #[cfg(target_os = "windows")]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

            // The dialog is modeless and owned by the main window, so it stays in front of it
            // without disabling it: the image keeps rendering and the slideshow keeps running
            // while the settings are open.
            let Some(window) = &self.window else {
                log::debug!("Settings: there's no window to own the dialog yet");
                return;
            };
            let Ok(RawWindowHandle::Win32(handle)) = window.window_handle().map(|h| h.as_raw())
            else {
                log::error!("Settings: no Win32 handle for the main window");
                return;
            };
            let owner =
                windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut std::ffi::c_void);
            crate::settings::show_settings_window(owner);
        }
    }

    /// Open the About box. Each platform builds its own (`about/CLAUDE.md`); all this does is
    /// hand it the main window to sit on top of and be centered on.
    fn show_about_dialog(&self) {
        #[cfg(target_os = "windows")]
        crate::about::windows::show_window(self.window.as_deref());

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

            crate::about::macos::show_window(parent_ptr);
        }
    }

    /// Print the current image via the system print dialog. Both platforms name the viewer
    /// window as the dialog's parent and hand the file off to their own print module; neither
    /// blocks the event loop (macOS runs an async sheet, Windows a worker thread).
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn print_current_image(&mut self) {
        let Some(path) = self
            .navigation
            .dir_list
            .as_ref()
            .map(|d| d.current().to_path_buf())
        else {
            log::debug!("Print: no image open");
            return;
        };
        #[cfg(target_os = "macos")]
        self.print_on_macos(&path);
        #[cfg(target_os = "windows")]
        {
            let Some(owner) = self.main_window_hwnd() else {
                log::warn!("Print: no viewer window to own the print dialog");
                return;
            };
            crate::platform::windows::print::print_image_file(
                &path,
                owner,
                self.raw_flags,
                self.color.relative_col,
            );
        }
    }

    /// Present the print sheet on the viewer window. Mirrors the NSWindow-pointer extraction
    /// used by the About/Settings dialogs, then hands off to the print module.
    #[cfg(target_os = "macos")]
    fn print_on_macos(&mut self, path: &std::path::Path) {
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

        if parent_ptr.is_null() {
            log::warn!("Print: no viewer window to attach the print sheet to");
            return;
        }

        // SAFETY: parent_ptr is a valid, live NSWindow* for the duration of this call.
        let parent_window = unsafe { &*parent_ptr };
        self._active_print = crate::platform::macos::print::print_image_file(path, parent_window);
        if self._active_print.is_some() {
            log::info!("Printing image: {}", path.display());
        }
    }

    fn handle_menu_event(&mut self) {
        let Some(app_menu) = &self.app_menu else {
            return;
        };
        if let Some(command) = app_menu.poll_command() {
            let _ = self.event_loop_proxy.send_event(command);
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

        // A drop's paths all arrive within one turn of the loop, so by the time the loop is
        // about to wait the batch is complete and can be opened as one request.
        if !self.pending_drops.is_empty() {
            let paths = std::mem::take(&mut self.pending_drops);
            self.execute_command(event_loop, AppCommand::OpenDropped(paths));
        }

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

        // "Loading…" overlay: reveal it once the pending image has outlived the delay, hide it the
        // moment the image lands. Only a change asks for a redraw, so an idle wait stays idle.
        let overlay_due = self.loading_overlay_due();
        if overlay_due != self.loading_overlay_visible {
            self.loading_overlay_visible = overlay_due;
            self.request_redraw();
        }

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

            // The only signal Windows gives that the window may have crossed onto another
            // monitor, and it arrives for every pixel of a title-bar drag. `refresh_display_
            // profile_after_move` keeps the cheap question (which monitor is this?) on that path
            // and the expensive one (read and parse its ICC profile) off it. macOS is told
            // instead, through `AppCommand::DisplayChanged`, so this costs it nothing.
            WindowEvent::Moved(_) => self.refresh_display_profile_after_move(),

            WindowEvent::Resized(size) => {
                log::debug!("Window resized to {}x{}", size.width, size.height);
                // The Windows browser is a child window covering the client area, so winit's own
                // resize doesn't reach it: it's told here. macOS pins its split view to the
                // content view's edges, so it follows on its own.
                #[cfg(target_os = "windows")]
                self.browser.relayout();
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
                let rendered = self.render_frame();
                if rendered {
                    self.needs_redraw = false;
                } else if let Some(win) = &self.window {
                    win.request_redraw();
                }
            }

            // A drag carrying files entered the window. winit sends one of these per file and
            // no event when the batch ends, so the pill goes up on the first and the paths are
            // ignored until the drop: telling the user whether Prvw can open them would mean
            // stat-ing every file of a drag that may never land.
            WindowEvent::HoveredFile(_) => {
                if !self.files_hovering {
                    self.files_hovering = true;
                    self.request_redraw();
                }
            }

            // The drag left without dropping.
            WindowEvent::HoveredFileCancelled => {
                if self.files_hovering {
                    self.files_hovering = false;
                    self.request_redraw();
                }
            }

            // One file of a drop that landed. The batch is opened in `about_to_wait`, once
            // every path of it has arrived.
            WindowEvent::DroppedFile(path) => {
                self.files_hovering = false;
                self.pending_drops.push(path);
                self.request_redraw();
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

            // Scroll: zoom (when "Scroll to zoom" is on or the platform's zoom modifier is
            // held) or move through the folder. `crate::scroll` owns every per-platform
            // decision in that sentence.
            WindowEvent::MouseWheel { delta, .. } => {
                // In browse mode the wheel belongs to whatever is under it — the thumbnail grid
                // or the folder tree — and must never zoom the hidden image or step through the
                // folder. The native panes scroll themselves; this is the guard for a wheel that
                // reaches winit anyway (over a pane's own gap, or before a pane has focus).
                if self.browser.is_browse() {
                    return;
                }
                match self.scroll.interpret(
                    delta,
                    self.scale_factor,
                    &self.modifiers,
                    self.zoom.scroll_to_zoom,
                ) {
                    Some(scroll::ScrollAction::Zoom(steps)) => {
                        // Zoom centered on cursor (Y offset into image area)
                        let old_zoom = self.zoom.view.zoom;
                        let (cx, cy) = self.last_mouse_pos;
                        let offset = Logical(self.content_offset_y().0 as f64);
                        self.zoom
                            .view
                            .scroll_zoom(steps, cx.as_f32(), (cy - offset).as_f32());
                        if self.zoom.auto_fit {
                            self.auto_fit_after_zoom(old_zoom, cx, cy);
                        }
                        self.update_transform_and_redraw();
                    }
                    Some(scroll::ScrollAction::Navigate(images)) => {
                        // Debounced, so a wheel spin collapses into one jump and one decode.
                        let forward = images > 0;
                        for _ in 0..images.abs() {
                            self.execute_command(
                                event_loop,
                                AppCommand::NavigateDebounced(forward),
                            );
                        }
                    }
                    None => {}
                }
            }

            // Trackpad pinch-to-zoom, cursor-centered. macOS and iOS only: winit reports no
            // gesture events elsewhere. Windows needs no counterpart because a precision
            // touchpad synthesises Ctrl + wheel for a pinch, which the arm above already zooms
            // with; Linux has neither, so Ctrl + scroll is the zoom there.
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
                    // In the "nothing open yet" empty state the whole canvas is the way in, and
                    // the overlay says so. There's no image to pan or fit, so nothing below
                    // this wants the click.
                    if self.empty_state == Some(EmptyState::NothingOpen) {
                        self.execute_command(event_loop, AppCommand::ShowOpenDialog);
                        return;
                    }
                    let now = Instant::now();
                    if let Some(last) = self.last_click_time
                        && now.duration_since(last) < crate::platform::double_click_interval()
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
                #[cfg(any(target_os = "macos", target_os = "windows"))]
                self.show_image_context_menu();
            }

            // The window moved to a monitor with a different scale factor, or the one it's on
            // changed. Windows sends this whenever a window crosses between a 150% laptop panel
            // and a 100% external monitor (the app manifest asks for per-monitor v2 awareness, so
            // the system reports the change instead of bitmap-stretching); macOS sends it moving
            // between a Retina display and a 1x one. A `Resized` follows, since the physical size
            // changes with the factor, but everything measured in logical pixels has to adopt the
            // new factor first or that resize is computed against the old one.
            WindowEvent::ScaleFactorChanged {
                scale_factor: new_scale,
                ..
            } => {
                self.scale_factor = new_scale;
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_scale_factor(new_scale);
                    if let Some((iw, ih)) = self.navigation.current_image_size {
                        self.zoom.view.update_dimensions(
                            iw,
                            ih,
                            renderer.logical_width(),
                            renderer.logical_height(),
                        );
                    }
                }
                self.update_min_zoom();
                if let Some(renderer) = &self.renderer {
                    renderer.update_transform(&self.zoom.view.transform());
                }
                // Every measurement the Windows browser holds is in device pixels at one
                // monitor's DPI: the fonts, the pane widths, and the image list's slot size.
                #[cfg(target_os = "windows")]
                self.browser.rescale();
                self.request_redraw();
                self.update_shared_state();
                log::debug!("Scale factor changed to {new_scale}");
            }

            _ => {}
        }
    }
}
