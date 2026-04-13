//! Tracks zoom level and pan offset for the current image view.
//!
//! Zoom is relative to fit-to-window: zoom=1.0 means the image fits the window,
//! zoom=2.0 means 2x magnification. The minimum zoom is 1.0 (can't zoom out past fit).
//! Pan is in NDC-like coordinates: (0, 0) is centered.
//!
//! The QA server reads zoom/pan values for reporting. The actual rendering is handled
//! by the Tauri webview (CSS transforms in the frontend).

const MAX_ZOOM: f32 = 100.0;
const ZOOM_STEP: f32 = 1.15; // ~15% per scroll tick
const KEYBOARD_ZOOM_STEP: f32 = 1.25;

#[derive(Debug, Clone)]
pub struct ViewState {
    /// Current zoom multiplier relative to fit-to-window. 1.0 = image fits the window.
    pub zoom: f32,
    /// Pan offset in NDC. (0, 0) = centered.
    pub pan_x: f32,
    pub pan_y: f32,
    /// Image aspect ratio (width / height).
    image_aspect: f32,
    /// Window aspect ratio (width / height).
    window_aspect: f32,
    /// Image dimensions in pixels.
    image_width: u32,
    image_height: u32,
    /// Window dimensions in pixels.
    window_width: u32,
    window_height: u32,
}

impl ViewState {
    pub fn new() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            image_aspect: 1.0,
            window_aspect: 1.0,
            image_width: 1,
            image_height: 1,
            window_width: 1,
            window_height: 1,
        }
    }

    /// Recalculate the fit-to-window scale. Call when the image or window size changes.
    pub fn update_dimensions(
        &mut self,
        image_width: u32,
        image_height: u32,
        window_width: u32,
        window_height: u32,
    ) {
        self.image_width = image_width;
        self.image_height = image_height;
        self.window_width = window_width;
        self.window_height = window_height;

        self.image_aspect = image_width as f32 / image_height as f32;
        self.window_aspect = window_width as f32 / window_height as f32;

        // Re-clamp pan: a resize can make a previously-valid pan position invalid
        // (for example, shrinking the window while the image is off-center).
        self.clamp_pan();
    }

    /// Reset to fit-to-window (zoom 1.0, centered).
    pub fn fit_to_window(&mut self) {
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Set zoom to show the image at its native pixel size.
    pub fn actual_size(&mut self) {
        if self.window_width == 0 || self.window_height == 0 {
            return;
        }
        // At zoom=1.0, the image fits the window. For actual size, we need
        // 1 image pixel = 1 screen pixel.
        // The fit-to-window shows the image at a size where it fills the window.
        // The number of image pixels visible at fit is determined by which axis is limiting.
        let visible_width = if self.image_aspect > self.window_aspect {
            // Width-limited: all image width is visible
            self.image_width as f32
        } else {
            // Height-limited: visible width = window_aspect * image_height
            self.window_aspect * self.image_height as f32
        };
        // At fit, `visible_width` image pixels span the window width (`window_width` screen pixels).
        // For 1:1, we need `window_width` image pixels to span the window.
        let actual_zoom = visible_width / self.window_width as f32;
        self.zoom = actual_zoom.clamp(1.0, MAX_ZOOM);
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Toggle between fit-to-window and actual size.
    pub fn toggle_fit(&mut self) {
        if self.window_width == 0 || self.window_height == 0 {
            return;
        }
        let visible_width = if self.image_aspect > self.window_aspect {
            self.image_width as f32
        } else {
            self.window_aspect * self.image_height as f32
        };
        let actual_zoom = visible_width / self.window_width as f32;
        if (self.zoom - actual_zoom).abs() < 0.01 {
            self.fit_to_window();
        } else {
            self.actual_size();
        }
    }

    /// Zoom by scroll wheel, centered on the cursor position.
    /// `cursor_x` and `cursor_y` are in pixels from top-left of the window.
    pub fn scroll_zoom(&mut self, delta: f32, cursor_x: f32, cursor_y: f32) {
        let factor = if delta > 0.0 {
            ZOOM_STEP
        } else {
            1.0 / ZOOM_STEP
        };
        self.zoom_around(factor, cursor_x, cursor_y);
    }

    /// Zoom in/out by keyboard shortcut (centered on window).
    pub fn keyboard_zoom(&mut self, zoom_in: bool) {
        let factor = if zoom_in {
            KEYBOARD_ZOOM_STEP
        } else {
            1.0 / KEYBOARD_ZOOM_STEP
        };
        let cx = self.window_width as f32 / 2.0;
        let cy = self.window_height as f32 / 2.0;
        self.zoom_around(factor, cx, cy);
    }

    /// Apply zoom factor centered on a specific cursor position (in window pixels).
    fn zoom_around(&mut self, factor: f32, cursor_x: f32, cursor_y: f32) {
        // Clamp: can't zoom out below fit-to-window (1.0)
        let new_zoom = (self.zoom * factor).clamp(1.0, MAX_ZOOM);
        if (new_zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }

        // Convert cursor position to NDC (-1..1), accounting for aspect ratio
        let ndc_x = (cursor_x / self.window_width as f32) * 2.0 - 1.0;
        let ndc_y = -((cursor_y / self.window_height as f32) * 2.0 - 1.0);

        // Adjust pan so the point under the cursor stays fixed
        let ratio = 1.0 - new_zoom / self.zoom;
        self.pan_x += (ndc_x - self.pan_x) * ratio;
        self.pan_y += (ndc_y - self.pan_y) * ratio;

        self.zoom = new_zoom;
        self.clamp_pan();
    }

    /// Pan by pixel delta (from mouse drag). Positive dx = move image right.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        if self.window_width == 0 || self.window_height == 0 {
            return;
        }
        self.pan_x += (dx / self.window_width as f32) * 2.0;
        self.pan_y -= (dy / self.window_height as f32) * 2.0;
        self.clamp_pan();
    }

    /// Clamp pan so the image edges don't leave the window.
    /// At fit-to-window (zoom=1.0), the image exactly fills one axis, so pan is 0.
    /// When zoomed in, the image can be panned but not past its edges.
    fn clamp_pan(&mut self) {
        let (sx, sy) = if self.image_aspect > self.window_aspect {
            (
                self.zoom,
                self.zoom * self.window_aspect / self.image_aspect,
            )
        } else {
            (
                self.zoom * self.image_aspect / self.window_aspect,
                self.zoom,
            )
        };

        // The image spans [-sx, sx] in NDC (before pan). The window spans [-1, 1].
        // With pan, the image spans [-sx + pan, sx + pan].
        // To keep the image covering the window (when zoomed in): -sx + pan <= -1 and sx + pan >= 1
        // Simplified: pan >= -(sx - 1) and pan <= (sx - 1)
        // When sx <= 1 (image smaller than window on this axis): allow free movement within the gap
        // but don't let the image go off-screen.
        let max_pan_x = if sx > 1.0 { sx - 1.0 } else { 1.0 - sx };
        let max_pan_y = if sy > 1.0 { sy - 1.0 } else { 1.0 - sy };

        self.pan_x = self.pan_x.clamp(-max_pan_x, max_pan_x);
        self.pan_y = self.pan_y.clamp(-max_pan_y, max_pan_y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_to_window_resets_zoom_and_pan() {
        let mut view = ViewState::new();
        view.zoom = 3.0;
        view.pan_x = 0.5;
        view.pan_y = -0.3;
        view.fit_to_window();
        assert_eq!(view.zoom, 1.0);
        assert_eq!(view.pan_x, 0.0);
        assert_eq!(view.pan_y, 0.0);
    }

    #[test]
    fn update_dimensions_wider_image() {
        let mut view = ViewState::new();
        // 1600x900 image (wide) in an 800x800 square window
        view.update_dimensions(1600, 900, 800, 800);
        // image_aspect > window_aspect, so width is the limiting axis at zoom=1
        assert_eq!(view.zoom, 1.0);
        // Aspect ratio stored correctly
        assert!((view.image_aspect - 1600.0 / 900.0).abs() < 0.01);
    }

    #[test]
    fn update_dimensions_taller_image() {
        let mut view = ViewState::new();
        // 600x1200 image (tall) in an 800x800 square window
        view.update_dimensions(600, 1200, 800, 800);
        assert_eq!(view.zoom, 1.0);
        assert!((view.image_aspect - 0.5).abs() < 0.01);
    }

    #[test]
    fn aspect_ratio_preserved_after_resize() {
        let mut view = ViewState::new();
        // 1600x900 image in a square window, then resized
        view.update_dimensions(1600, 900, 800, 800);
        let aspect_1 = view.image_aspect;

        // Resize to wide window: image aspect should be unchanged
        view.update_dimensions(1600, 900, 1200, 600);
        let aspect_2 = view.image_aspect;
        assert!((aspect_1 - aspect_2).abs() < 0.001);
    }

    #[test]
    fn zoom_clamped_to_fit_minimum() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, 800, 600);
        // Zoom out a lot
        for _ in 0..200 {
            view.keyboard_zoom(false);
        }
        // Should never go below 1.0 (fit to window)
        assert!((view.zoom - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn zoom_clamped_to_max() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, 800, 600);
        for _ in 0..200 {
            view.keyboard_zoom(true);
        }
        assert!(view.zoom <= MAX_ZOOM);
    }

    #[test]
    fn keyboard_zoom_in_increases_zoom() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, 800, 600);
        let original = view.zoom;
        view.keyboard_zoom(true);
        assert!(view.zoom > original);
    }

    #[test]
    fn pan_moves_offset_when_zoomed_in() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, 800, 600);
        // Must zoom in to have room to pan
        view.zoom = 3.0;
        view.pan(100.0, 0.0);
        assert!(view.pan_x > 0.0);
        assert_eq!(view.pan_y, 0.0);
    }

    #[test]
    fn pan_clamped_at_fit_zoom() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 800, 800, 800);
        // At zoom=1.0 with square image in square window, no room to pan
        view.pan(100.0, 0.0);
        assert!((view.pan_x).abs() < f32::EPSILON);
    }

    #[test]
    fn scroll_zoom_at_center_preserves_pan() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, 800, 600);
        view.scroll_zoom(1.0, 400.0, 300.0);
        assert!(view.pan_x.abs() < 0.01);
        assert!(view.pan_y.abs() < 0.01);
    }

    #[test]
    fn zoom_value_persists() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 800, 800, 800);
        view.zoom = 2.0;
        assert!((view.zoom - 2.0).abs() < 0.01);
    }

    #[test]
    fn toggle_fit_switches_between_fit_and_actual() {
        let mut view = ViewState::new();
        view.update_dimensions(1600, 900, 800, 600);
        assert_eq!(view.zoom, 1.0);
        view.toggle_fit();
        assert!(view.zoom > 1.0);
        view.toggle_fit();
        assert_eq!(view.zoom, 1.0);
    }
}
