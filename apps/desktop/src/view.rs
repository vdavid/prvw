//! Tracks zoom level and pan offset for the current image view.
//!
//! Zoom is in logical pixels: 1.0 = one image pixel per logical pixel (point).
//! On Retina (2x), this means 1 image pixel = 2 physical pixels.
//! Values below 1.0 shrink the image, above 1.0 magnify it. The minimum zoom
//! is the fit-to-window level. All window dimensions are logical pixels.

use crate::pixels::Logical;

const MAX_ZOOM: f32 = 100.0;
const ZOOM_STEP: f32 = 1.05; // ~5% per scroll tick
const KEYBOARD_ZOOM_STEP: f32 = 1.25;

#[derive(Debug, Clone)]
pub struct ViewState {
    /// Zoom level: 1.0 = one image pixel per logical pixel (100%). 2.0 = 200%, etc.
    pub zoom: f32,
    /// Pan offset in NDC. (0, 0) = centered.
    pub pan_x: f32,
    pub pan_y: f32,
    /// Minimum zoom level (the zoom that fits the image in the window, or 1.0 for
    /// small images when enlargement is disabled).
    min_zoom: f32,
    /// Image dimensions in native pixels.
    image_width: u32,
    image_height: u32,
    /// Window dimensions in logical pixels (points).
    window_width: Logical<f32>,
    window_height: Logical<f32>,
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
            min_zoom: 1.0,
            image_width: 1,
            image_height: 1,
            window_width: Logical(1.0),
            window_height: Logical(1.0),
        }
    }

    /// Update image and window dimensions. Window dimensions must be in logical pixels.
    pub fn update_dimensions(
        &mut self,
        image_width: u32,
        image_height: u32,
        window_width: Logical<f32>,
        window_height: Logical<f32>,
    ) {
        self.image_width = image_width;
        self.image_height = image_height;
        self.window_width = window_width;
        self.window_height = window_height;
        self.clamp_pan();
    }

    /// The zoom level at which the image exactly fits the window (both axes visible).
    pub fn fit_zoom(&self) -> f32 {
        if self.image_width == 0
            || self.image_height == 0
            || self.window_width.0 == 0.0
            || self.window_height.0 == 0.0
        {
            return 1.0;
        }
        let scale_x = self.window_width.0 / self.image_width as f32;
        let scale_y = self.window_height.0 / self.image_height as f32;
        scale_x.min(scale_y)
    }

    /// Set zoom to fit the image in the window, centered.
    pub fn fit_to_window(&mut self) {
        self.zoom = self.fit_zoom();
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Set zoom to 1:1 logical pixel mapping (100%, actual size), centered.
    pub fn actual_size(&mut self) {
        self.zoom = 1.0_f32.clamp(self.min_zoom, MAX_ZOOM);
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Set zoom to an absolute level, clamped to min_zoom..MAX_ZOOM, centered.
    pub fn set_zoom(&mut self, level: f32) {
        self.zoom = level.clamp(self.min_zoom, MAX_ZOOM);
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Set the minimum zoom floor. Reclamps the current zoom if it's below the new floor.
    pub fn set_min_zoom(&mut self, min: f32) {
        self.min_zoom = min;
        if self.zoom < self.min_zoom {
            self.zoom = self.min_zoom;
        }
    }

    /// Toggle between fit-to-window and actual size.
    pub fn toggle_fit(&mut self) {
        if (self.zoom - 1.0).abs() < 0.01 {
            self.fit_to_window();
        } else {
            self.actual_size();
        }
    }

    /// Zoom by scroll wheel, centered on the cursor position.
    pub fn scroll_zoom(&mut self, delta: f32, cursor_x: Logical<f32>, cursor_y: Logical<f32>) {
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
        let cx = self.window_width / 2.0;
        let cy = self.window_height / 2.0;
        self.zoom_around(factor, cx, cy);
    }

    /// Apply zoom factor centered on a specific cursor position (in logical pixels).
    fn zoom_around(&mut self, factor: f32, cursor_x: Logical<f32>, cursor_y: Logical<f32>) {
        let new_zoom = (self.zoom * factor).clamp(self.min_zoom, MAX_ZOOM);
        if (new_zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }

        // Convert cursor position to NDC (-1..1)
        let ndc_x = (cursor_x.0 / self.window_width.0) * 2.0 - 1.0;
        let ndc_y = -((cursor_y.0 / self.window_height.0) * 2.0 - 1.0);

        // Adjust pan so the point under the cursor stays fixed
        let ratio = 1.0 - new_zoom / self.zoom;
        self.pan_x += (ndc_x - self.pan_x) * ratio;
        self.pan_y += (ndc_y - self.pan_y) * ratio;

        self.zoom = new_zoom;
        self.clamp_pan();
    }

    /// Pan by logical pixel delta (from mouse drag). Positive dx = move image right.
    pub fn pan(&mut self, dx: Logical<f32>, dy: Logical<f32>) {
        if self.window_width.0 == 0.0 || self.window_height.0 == 0.0 {
            return;
        }
        self.pan_x += (dx.0 / self.window_width.0) * 2.0;
        self.pan_y -= (dy.0 / self.window_height.0) * 2.0;
        self.clamp_pan();
    }

    /// Clamp pan so the image edges don't leave the window.
    fn clamp_pan(&mut self) {
        // Compute the NDC half-extents of the image quad.
        // sx = (image_width * zoom) / window_width, sy = (image_height * zoom) / window_height
        let sx = self.image_width as f32 * self.zoom / self.window_width.0;
        let sy = self.image_height as f32 * self.zoom / self.window_height.0;

        // When zoomed in (sx > 1): keep image edges covering the window.
        // When zoomed out (sx <= 1): center the image — no panning on that axis.
        let max_pan_x = if sx > 1.0 { sx - 1.0 } else { 0.0 };
        let max_pan_y = if sy > 1.0 { sy - 1.0 } else { 0.0 };

        self.pan_x = self.pan_x.clamp(-max_pan_x, max_pan_x);
        self.pan_y = self.pan_y.clamp(-max_pan_y, max_pan_y);
    }

    /// Build the transform uniform to send to the GPU.
    pub fn transform(&self) -> TransformUniform {
        // The image quad spans [-1, 1] on both axes in NDC.
        // Scale it so that `zoom` image pixels = `zoom` screen pixels.
        // sx = (image_width * zoom) / window_width
        // sy = (image_height * zoom) / window_height
        let sx = self.image_width as f32 * self.zoom / self.window_width.0;
        let sy = self.image_height as f32 * self.zoom / self.window_height.0;

        TransformUniform {
            col0: [sx, 0.0, 0.0, sy],
            col1: [self.pan_x, self.pan_y, 0.0, 0.0],
        }
    }

    /// Compute the rendered image rectangle in logical pixels: (x, y, width, height).
    pub fn rendered_rect(&self) -> (Logical<f32>, Logical<f32>, Logical<f32>, Logical<f32>) {
        let t = self.transform();
        let sx = t.col0[0];
        let sy = t.col0[3];
        let tx = t.col1[0];
        let ty = t.col1[1];
        let left = ((tx - sx + 1.0) / 2.0) * self.window_width.0;
        let right = ((tx + sx + 1.0) / 2.0) * self.window_width.0;
        let top = ((1.0 - ty - sy) / 2.0) * self.window_height.0;
        let bottom = ((1.0 - ty + sy) / 2.0) * self.window_height.0;
        (
            Logical(left),
            Logical(top),
            Logical(right - left),
            Logical(bottom - top),
        )
    }

    /// Get the current minimum zoom value.
    pub fn min_zoom_value(&self) -> f32 {
        self.min_zoom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_to_window_computes_correct_zoom() {
        let mut view = ViewState::new();
        // 1600x900 image in 800x800 logical window: fit zoom = 800/1600 = 0.5
        view.update_dimensions(1600, 900, Logical(800.0), Logical(800.0));
        view.fit_to_window();
        assert!((view.zoom - 0.5).abs() < 0.01);
        assert_eq!(view.pan_x, 0.0);
        assert_eq!(view.pan_y, 0.0);
    }

    #[test]
    fn actual_size_is_zoom_1() {
        let mut view = ViewState::new();
        view.update_dimensions(1600, 900, Logical(800.0), Logical(600.0));
        view.set_min_zoom(0.1);
        view.actual_size();
        assert!((view.zoom - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn fit_zoom_for_wider_image() {
        let mut view = ViewState::new();
        // 1600x900 in 800x800: limited by width, fit = 800/1600 = 0.5
        view.update_dimensions(1600, 900, Logical(800.0), Logical(800.0));
        assert!((view.fit_zoom() - 0.5).abs() < 0.01);
    }

    #[test]
    fn fit_zoom_for_taller_image() {
        let mut view = ViewState::new();
        // 600x1200 in 800x800: limited by height, fit = 800/1200 = 0.667
        view.update_dimensions(600, 1200, Logical(800.0), Logical(800.0));
        assert!((view.fit_zoom() - 0.667).abs() < 0.01);
    }

    #[test]
    fn transform_at_fit_fills_one_axis() {
        let mut view = ViewState::new();
        // 1600x900 in 800x800: at fit (zoom=0.5), width fills window
        view.update_dimensions(1600, 900, Logical(800.0), Logical(800.0));
        view.fit_to_window();
        let t = view.transform();
        // sx = 1600 * 0.5 / 800 = 1.0 (fills width)
        assert!((t.col0[0] - 1.0).abs() < 0.01);
        // sy = 900 * 0.5 / 800 = 0.5625 (shorter)
        assert!(t.col0[3] < 1.0);
    }

    #[test]
    fn aspect_ratio_preserved_after_resize() {
        let mut view = ViewState::new();
        let image_aspect: f32 = 1600.0 / 900.0;

        view.update_dimensions(1600, 900, Logical(800.0), Logical(800.0));
        view.fit_to_window();
        let t1 = view.transform();
        let rendered_aspect_1 = (t1.col0[0] / t1.col0[3]) * (800.0 / 800.0);
        assert!((rendered_aspect_1 - image_aspect).abs() < 0.01);

        view.update_dimensions(1600, 900, Logical(1200.0), Logical(600.0));
        view.fit_to_window();
        let t2 = view.transform();
        let rendered_aspect_2 = (t2.col0[0] / t2.col0[3]) * (1200.0 / 600.0);
        assert!((rendered_aspect_2 - image_aspect).abs() < 0.01);
    }

    #[test]
    fn zoom_clamped_to_fit_minimum() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, Logical(800.0), Logical(600.0));
        view.fit_to_window();
        view.set_min_zoom(view.fit_zoom());
        for _ in 0..200 {
            view.keyboard_zoom(false);
        }
        assert!((view.zoom - view.fit_zoom()).abs() < f32::EPSILON);
    }

    #[test]
    fn zoom_clamped_to_max() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, Logical(800.0), Logical(600.0));
        for _ in 0..200 {
            view.keyboard_zoom(true);
        }
        assert!(view.zoom <= MAX_ZOOM);
    }

    #[test]
    fn keyboard_zoom_in_increases_zoom() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, Logical(800.0), Logical(600.0));
        view.fit_to_window();
        let original = view.zoom;
        view.keyboard_zoom(true);
        assert!(view.zoom > original);
    }

    #[test]
    fn pan_moves_offset_when_zoomed_in() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, Logical(800.0), Logical(600.0));
        // zoom=3 means 3x actual pixels — image is much bigger than window
        view.zoom = 3.0;
        view.pan(Logical(100.0), Logical(0.0));
        assert!(view.pan_x > 0.0);
        assert_eq!(view.pan_y, 0.0);
    }

    #[test]
    fn pan_clamped_when_image_smaller_than_window() {
        let mut view = ViewState::new();
        // 200x200 image in 800x800 window at zoom=1.0 (actual size): image is smaller
        view.update_dimensions(200, 200, Logical(800.0), Logical(800.0));
        view.zoom = 1.0;
        view.pan(Logical(100.0), Logical(0.0));
        assert!((view.pan_x).abs() < f32::EPSILON);
    }

    #[test]
    fn scroll_zoom_at_center_preserves_pan() {
        let mut view = ViewState::new();
        view.update_dimensions(800, 600, Logical(800.0), Logical(600.0));
        view.fit_to_window();
        view.scroll_zoom(1.0, Logical(400.0), Logical(300.0));
        assert!(view.pan_x.abs() < 0.01);
        assert!(view.pan_y.abs() < 0.01);
    }

    #[test]
    fn transform_at_actual_size() {
        let mut view = ViewState::new();
        // 800x800 image in 800x800 window at zoom=1.0: sx = sy = 1.0
        view.update_dimensions(800, 800, Logical(800.0), Logical(800.0));
        view.zoom = 1.0;
        let t = view.transform();
        assert!((t.col0[0] - 1.0).abs() < 0.01);
        assert!((t.col0[3] - 1.0).abs() < 0.01);
    }

    #[test]
    fn toggle_fit_switches_between_fit_and_actual() {
        let mut view = ViewState::new();
        view.update_dimensions(1600, 900, Logical(800.0), Logical(600.0));
        view.set_min_zoom(0.1);
        view.fit_to_window();
        let fit = view.zoom;
        assert!(fit < 1.0); // fit zoom for a large image is < 1.0
        view.toggle_fit();
        assert!((view.zoom - 1.0).abs() < 0.01); // toggled to actual size
        view.toggle_fit();
        assert!((view.zoom - fit).abs() < 0.01); // toggled back to fit
    }

    #[test]
    fn small_image_min_zoom_at_actual_size() {
        let mut view = ViewState::new();
        // 200x200 in 800x800: fit_zoom = 800/200 = 4.0, actual = 1.0
        view.update_dimensions(200, 200, Logical(800.0), Logical(800.0));
        // With enlarge off, min_zoom = 1.0 (don't enlarge)
        view.set_min_zoom(1.0);
        view.actual_size();
        assert!((view.zoom - 1.0).abs() < f32::EPSILON);
        // Can't zoom out past 1.0
        for _ in 0..100 {
            view.keyboard_zoom(false);
        }
        assert!((view.zoom - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn small_image_fit_zooms_above_one() {
        let mut view = ViewState::new();
        // 200x200 in 800x800: fit_zoom = 4.0
        view.update_dimensions(200, 200, Logical(800.0), Logical(800.0));
        view.fit_to_window();
        assert!((view.zoom - 4.0).abs() < 0.01);
    }

    #[test]
    fn set_min_zoom_reclamps() {
        let mut view = ViewState::new();
        view.zoom = 0.5;
        view.set_min_zoom(1.0);
        assert!((view.zoom - 1.0).abs() < f32::EPSILON);
    }
}
