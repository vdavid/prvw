//! Tracks zoom level and pan offset for the current image view.
//! All values are in normalized device coordinates (NDC): the window spans [-1, 1] on both axes.
//! Zoom and pan are GPU-only transforms (no re-decode needed).

const MIN_ZOOM: f32 = 0.01;
const MAX_ZOOM: f32 = 100.0;
const ZOOM_STEP: f32 = 1.15; // ~15% per scroll tick or keypress
const KEYBOARD_ZOOM_STEP: f32 = 1.25;

#[derive(Debug, Clone)]
pub struct ViewState {
    /// Current zoom multiplier relative to fit-to-window. 1.0 = image fits the window.
    pub zoom: f32,
    /// Pan offset in NDC. (0, 0) = centered.
    pub pan_x: f32,
    pub pan_y: f32,
    /// The scale that makes the image fit the window (depends on image and window dimensions).
    fit_scale_x: f32,
    fit_scale_y: f32,
    /// Image dimensions in pixels.
    image_width: u32,
    image_height: u32,
    /// Window dimensions in pixels.
    window_width: u32,
    window_height: u32,
}

/// The transform data sent to the GPU uniform buffer.
/// Layout matches the shader's Transform struct.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TransformUniform {
    pub col0: [f32; 4], // (scale_x, 0, 0, scale_y)
    pub col1: [f32; 4], // (translate_x, translate_y, 0, 0)
}

impl ViewState {
    pub fn new() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            fit_scale_x: 1.0,
            fit_scale_y: 1.0,
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

        let aspect_image = image_width as f32 / image_height as f32;
        let aspect_window = window_width as f32 / window_height as f32;

        if aspect_image > aspect_window {
            // Image is wider than window: fit to width
            self.fit_scale_x = 1.0;
            self.fit_scale_y = aspect_window / aspect_image;
        } else {
            // Image is taller than window: fit to height
            self.fit_scale_x = aspect_image / aspect_window;
            self.fit_scale_y = 1.0;
        }
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
        // At zoom=1.0, image fits window. "Actual size" means 1 image pixel = 1 screen pixel.
        let fit_width_pixels = self.fit_scale_x * self.window_width as f32;
        let actual_zoom = self.image_width as f32 / fit_width_pixels;
        self.zoom = actual_zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Toggle between fit-to-window and actual size.
    pub fn toggle_fit(&mut self) {
        let fit_width_pixels = self.fit_scale_x * self.window_width as f32;
        let actual_zoom = self.image_width as f32 / fit_width_pixels;
        // If we're close to actual size, go to fit. Otherwise go to actual size.
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
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (new_zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }

        // Convert cursor position to NDC (-1..1)
        let ndc_x = (cursor_x / self.window_width as f32) * 2.0 - 1.0;
        let ndc_y = -((cursor_y / self.window_height as f32) * 2.0 - 1.0);

        // Adjust pan so the point under the cursor stays fixed
        let ratio = 1.0 - new_zoom / self.zoom;
        self.pan_x += (ndc_x - self.pan_x) * ratio;
        self.pan_y += (ndc_y - self.pan_y) * ratio;

        self.zoom = new_zoom;
    }

    /// Pan by pixel delta (from mouse drag). Positive dx = move image right.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.pan_x += (dx / self.window_width as f32) * 2.0;
        self.pan_y -= (dy / self.window_height as f32) * 2.0;
    }

    /// Build the transform uniform to send to the GPU.
    pub fn transform(&self) -> TransformUniform {
        let sx = self.fit_scale_x * self.zoom;
        let sy = self.fit_scale_y * self.zoom;
        TransformUniform {
            col0: [sx, 0.0, 0.0, sy],
            col1: [self.pan_x, self.pan_y, 0.0, 0.0],
        }
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
        // 1600x900 image in a 800x800 window (image is wider)
        view.update_dimensions(1600, 900, 800, 800);
        assert!((view.fit_scale_x - 1.0).abs() < f32::EPSILON);
        assert!(view.fit_scale_y < 1.0);
    }

    #[test]
    fn update_dimensions_taller_image() {
        let mut view = ViewState::new();
        // 600x1200 image in a 800x800 window (image is taller)
        view.update_dimensions(600, 1200, 800, 800);
        assert!(view.fit_scale_x < 1.0);
        assert!((view.fit_scale_y - 1.0).abs() < f32::EPSILON);
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
    fn keyboard_zoom_out_decreases_zoom() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, 800, 600);
        let original = view.zoom;
        view.keyboard_zoom(false);
        assert!(view.zoom < original);
    }

    #[test]
    fn zoom_clamped_to_bounds() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, 800, 600);
        // Zoom out a lot
        for _ in 0..200 {
            view.keyboard_zoom(false);
        }
        assert!(view.zoom >= MIN_ZOOM);

        // Zoom in a lot
        for _ in 0..200 {
            view.keyboard_zoom(true);
        }
        assert!(view.zoom <= MAX_ZOOM);
    }

    #[test]
    fn pan_moves_offset() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, 800, 600);
        view.pan(100.0, 0.0);
        assert!(view.pan_x > 0.0);
        assert_eq!(view.pan_y, 0.0);
    }

    #[test]
    fn scroll_zoom_preserves_point_under_cursor() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, 800, 600);
        // Zoom at the center should not change pan
        view.scroll_zoom(1.0, 400.0, 300.0);
        assert!(view.pan_x.abs() < 0.01);
        assert!(view.pan_y.abs() < 0.01);
    }

    #[test]
    fn transform_reflects_zoom_and_fit() {
        let mut view = ViewState::new();
        view.update_dimensions(1600, 900, 800, 800);
        view.zoom = 2.0;
        let t = view.transform();
        assert!((t.col0[0] - 2.0).abs() < f32::EPSILON); // fit_scale_x=1.0 * zoom=2.0
        assert!(t.col0[3] < 2.0); // fit_scale_y < 1.0 * zoom=2.0
    }

    #[test]
    fn toggle_fit_switches_between_fit_and_actual() {
        let mut view = ViewState::new();
        view.update_dimensions(1600, 900, 800, 600);
        // Start at fit
        assert_eq!(view.zoom, 1.0);
        // Toggle to actual size
        view.toggle_fit();
        assert!(view.zoom > 1.0); // 1600px image in 800px-wide window needs zoom > 1 for actual size
        // Toggle back to fit
        view.toggle_fit();
        assert_eq!(view.zoom, 1.0);
    }
}
