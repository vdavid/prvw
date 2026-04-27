//! `SharedAppState` — the snapshot of app state that the QA server reads from another
//! thread.
//!
//! The struct is `Clone + Debug`. The main thread writes it via `App::update_shared_state()`
//! on every state change; the QA server thread reads it under an `Arc<Mutex<_>>`.

use super::App;
use crate::pixels::{from_logical_pos, from_logical_size};
use crate::window;
use std::path::PathBuf;

/// Snapshot of app state, updated by the main thread on every state change.
#[derive(Clone, Debug)]
pub struct SharedAppState {
    pub current_file: Option<PathBuf>,
    pub current_index: usize,
    pub total_files: usize,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    pub fullscreen: bool,
    pub window_x: f64,
    pub window_y: f64,
    pub window_width: u32,
    pub window_height: u32,
    pub window_title: String,
    pub image_width: u32,
    pub image_height: u32,
    pub image_render_x: f32,
    pub image_render_y: f32,
    pub image_render_width: f32,
    pub image_render_height: f32,
    pub min_zoom: f32,
    /// Whether auto-fit window is enabled.
    pub auto_fit_window: bool,
    /// Whether small images are enlarged to fill the window.
    pub enlarge_small_images: bool,
    /// Whether scroll zooms (true) or navigates images (false).
    pub scroll_to_zoom: bool,
    /// Whether the title bar area is reserved at the top.
    pub title_bar: bool,
    /// Whether the histogram overlay is visible.
    pub histogram_visible: bool,
    /// Currently hovered histogram bin (0..=255), or `None` when the cursor
    /// isn't over the histogram or the histogram isn't visible.
    pub histogram_hover_bin: Option<u8>,
    /// Whether the EXIF info overlay is visible (user-toggled). The overlay
    /// only renders when this **and** `exif_present` are true.
    pub exif_visible: bool,
    /// Whether the currently displayed image has any EXIF metadata. False
    /// for PNG, GIF, BMP, plain WebP, and JPEGs without an EXIF segment.
    pub exif_present: bool,
    /// Pre-formatted diagnostics text, updated by the main thread.
    pub diagnostics_text: String,
    /// Thumbnail scheduler status, mirrored from `thumbnails::State::status()`.
    /// Macos-only; on other platforms stays at defaults.
    pub thumbnails: ThumbnailsSnapshot,
}

/// Snapshot of the thumbnail scheduler, mirrored into shared state so the
/// QA server can expose it over MCP without locking the scheduler itself.
#[derive(Clone, Debug, Default)]
pub struct ThumbnailsSnapshot {
    pub folder_len: usize,
    pub current: usize,
    pub in_flight: Vec<usize>,
    pub queue_len: usize,
    pub cached: Vec<usize>,
    pub failed: Vec<usize>,
    pub paused: bool,
    pub max_parallel: usize,
    pub pending_current: Option<usize>,
    /// Whether the image texture currently shows a thumbnail placeholder
    /// (uploaded while `pending_current.is_some()` because a thumb was
    /// cached). Resets to false on full-decode arrival.
    pub placeholder_active: bool,
    /// Ring buffer of recent thumbnail-lifecycle events with monotonic
    /// millisecond timestamps since app start. Lets MCP clients
    /// reconstruct what happened even after the thumb-visible window
    /// has closed (solves "MCP too slow to catch the placeholder").
    pub events: Vec<ThumbnailEvent>,
}

#[derive(Clone, Debug)]
pub struct ThumbnailEvent {
    pub ts_ms: u64,
    pub kind: &'static str,
    pub detail: String,
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
            window_x: 0.0,
            window_y: 0.0,
            window_width: 0,
            window_height: 0,
            window_title: String::new(),
            image_width: 0,
            image_height: 0,
            image_render_x: 0.0,
            image_render_y: 0.0,
            image_render_width: 0.0,
            image_render_height: 0.0,
            min_zoom: 1.0,
            auto_fit_window: true,
            enlarge_small_images: false,
            scroll_to_zoom: false,
            title_bar: true,
            histogram_visible: false,
            histogram_hover_bin: None,
            exif_visible: false,
            exif_present: false,
            diagnostics_text: String::new(),
            thumbnails: ThumbnailsSnapshot::default(),
        }
    }
}

impl App {
    /// Push current app state into the shared mutex for the QA server to read.
    pub(super) fn update_shared_state(&self) {
        let Ok(mut state) = self.shared_state.lock() else {
            return;
        };

        state.zoom = self.zoom.view.zoom;
        state.pan_x = self.zoom.view.pan_x;
        state.pan_y = self.zoom.view.pan_y;
        state.auto_fit_window = self.zoom.auto_fit;
        state.enlarge_small_images = self.zoom.enlarge;
        state.scroll_to_zoom = self.zoom.scroll_to_zoom;
        state.title_bar = self.title_bar;
        state.histogram_visible = self.histogram.visible;
        state.histogram_hover_bin = self.histogram.hover_bin;
        state.exif_visible = self.exif_overlay.visible;
        state.exif_present = self.current_image_has_exif();

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

        if let Some(dir) = &self.navigation.dir_list {
            state.current_file = Some(dir.current().to_path_buf());
            state.current_index = dir.current_index();
            state.total_files = dir.len();
        }

        if let Some((iw, ih)) = self.navigation.current_image_size {
            state.image_width = iw;
            state.image_height = ih;
        }
        state.min_zoom = self.zoom.view.min_zoom_value();
        let (rx, ry, rw, rh) = self.zoom.view.rendered_rect();
        state.image_render_x = rx.0;
        state.image_render_y = ry.0;
        state.image_render_width = rw.0;
        state.image_render_height = rh.0;

        state.diagnostics_text = crate::diagnostics::build_text(
            &self.navigation.image_cache.diagnostics(),
            state.current_index,
            &self.navigation.history,
        );

        #[cfg(target_os = "macos")]
        {
            let status = self.thumbnails.status();
            state.thumbnails = ThumbnailsSnapshot {
                folder_len: status.folder_len,
                current: status.current,
                in_flight: status.in_flight,
                queue_len: status.queue_len,
                cached: status.cached,
                failed: status.failed,
                paused: status.paused,
                max_parallel: status.max_parallel,
                pending_current: self.navigation.pending_current,
                placeholder_active: self.placeholder_active,
                events: self.thumbnail_events.iter().cloned().collect(),
            };
        }
    }
}
