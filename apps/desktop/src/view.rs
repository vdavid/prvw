//! Tracks zoom level and pan offset for the current image view.
//!
//! The coordinate system: the window spans [-1, 1] in NDC on both axes. The image is rendered as a
//! textured quad that's scaled to maintain its aspect ratio (letterboxed/pillarboxed as needed).
//!
//! `fit_scale` is a single uniform value that makes the image exactly fit the window (the largest
//! scale where the entire image is visible). Zoom is relative to this: zoom=1.0 means fit-to-window,
//! zoom=2.0 means 2x magnification. The minimum zoom defaults to 1.0 but can be lowered to
//! allow small images to display at their native pixel size without enlargement.

const MAX_ZOOM: f32 = 100.0;
const ZOOM_STEP: f32 = 1.05; // ~5% per scroll tick
const KEYBOARD_ZOOM_STEP: f32 = 1.25;

#[derive(Debug, Clone)]
pub struct ViewState {
    /// Current zoom multiplier relative to fit-to-window. 1.0 = image fits the window.
    pub zoom: f32,
    /// Pan offset in NDC. (0, 0) = centered.
    pub pan_x: f32,
    pub pan_y: f32,
    /// Minimum zoom level. Defaults to 1.0 (fit-to-window). Lowered for small images
    /// when "Enlarge small images" is off, so they can display at native pixel size.
    min_zoom: f32,
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

    /// Compute the zoom level that gives 1:1 pixel mapping (image pixel = screen pixel).
    /// Can be < 1.0 for small images (meaning the image is smaller than fit-to-window).
    pub fn actual_size_zoom(&self) -> f32 {
        if self.window_width == 0 || self.window_height == 0 {
            return 1.0;
        }
        // At zoom=1.0, the image fits the window. For actual size, we need
        // 1 image pixel = 1 screen pixel.
        let visible_width = if self.image_aspect > self.window_aspect {
            self.image_width as f32
        } else {
            self.window_aspect * self.image_height as f32
        };
        visible_width / self.window_width as f32
    }

    /// Set zoom to show the image at its native pixel size.
    pub fn actual_size(&mut self) {
        let actual_zoom = self.actual_size_zoom();
        self.zoom = actual_zoom.clamp(self.min_zoom, MAX_ZOOM);
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
        let new_zoom = (self.zoom * factor).clamp(self.min_zoom, MAX_ZOOM);
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
        // When zoomed in (sx > 1): keep image edges covering the window.
        // When zoomed out (sx <= 1): center the image — no panning allowed on that axis,
        // because a small image floating off-center looks broken.
        let max_pan_x = if sx > 1.0 { sx - 1.0 } else { 0.0 };
        let max_pan_y = if sy > 1.0 { sy - 1.0 } else { 0.0 };

        self.pan_x = self.pan_x.clamp(-max_pan_x, max_pan_x);
        self.pan_y = self.pan_y.clamp(-max_pan_y, max_pan_y);
    }

    /// Build the transform uniform to send to the GPU.
    /// The scale preserves the image's aspect ratio: both axes are scaled uniformly,
    /// with aspect correction applied so the image is never stretched.
    pub fn transform(&self) -> TransformUniform {
        // The image quad spans [-1, 1] on both axes (a square in NDC).
        // We need to scale it to show the correct aspect ratio and fit the window.
        //
        // Step 1: aspect-correct the quad so it matches the image proportions.
        // Step 2: scale it so it fits the window (the larger dimension fills the window).
        // Step 3: apply user zoom.

        let (sx, sy) = if self.image_aspect > self.window_aspect {
            // Image wider than window: width fills window, height is smaller.
            // sx = zoom (full width at zoom=1)
            // sy = zoom * (window_aspect / image_aspect) (shorter)
            (
                self.zoom,
                self.zoom * self.window_aspect / self.image_aspect,
            )
        } else {
            // Image taller than window: height fills window, width is smaller.
            // sx = zoom * (image_aspect / window_aspect) (narrower)
            // sy = zoom (full height at zoom=1)
            (
                self.zoom * self.image_aspect / self.window_aspect,
                self.zoom,
            )
        };

        TransformUniform {
            col0: [sx, 0.0, 0.0, sy],
            col1: [self.pan_x, self.pan_y, 0.0, 0.0],
        }
    }

    /// Compute the rendered image rectangle in window pixels: (x, y, width, height).
    pub fn rendered_rect(&self) -> (f32, f32, f32, f32) {
        let t = self.transform();
        let sx = t.col0[0];
        let sy = t.col0[3];
        let tx = t.col1[0];
        let ty = t.col1[1];
        let left = ((tx - sx + 1.0) / 2.0) * self.window_width as f32;
        let right = ((tx + sx + 1.0) / 2.0) * self.window_width as f32;
        let top = ((1.0 - ty - sy) / 2.0) * self.window_height as f32;
        let bottom = ((1.0 - ty + sy) / 2.0) * self.window_height as f32;
        (left, top, right - left, bottom - top)
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
        view.update_dimensions(1600, 900, 800, 800);
        let t = view.transform();
        // Width fills window (sx=1.0), height is shorter
        assert!((t.col0[0] - 1.0).abs() < 0.01);
        assert!(t.col0[3] < 1.0);
    }

    #[test]
    fn update_dimensions_taller_image() {
        let mut view = ViewState::new();
        view.update_dimensions(600, 1200, 800, 800);
        let t = view.transform();
        // Height fills window (sy=1.0), width is narrower
        assert!(t.col0[0] < 1.0);
        assert!((t.col0[3] - 1.0).abs() < 0.01);
    }

    #[test]
    fn aspect_ratio_preserved_after_resize() {
        let mut view = ViewState::new();
        let image_aspect: f32 = 1600.0 / 900.0;

        // 1600x900 image in a square window
        view.update_dimensions(1600, 900, 800, 800);
        let t1 = view.transform();
        // The rendered aspect ratio (sx/sy * window_aspect) should match the image aspect
        let rendered_aspect_1 = (t1.col0[0] / t1.col0[3]) * (800.0 / 800.0);
        assert!((rendered_aspect_1 - image_aspect).abs() < 0.01);

        // Resize to wide window: aspect ratio should still be preserved
        view.update_dimensions(1600, 900, 1200, 600);
        let t2 = view.transform();
        let rendered_aspect_2 = (t2.col0[0] / t2.col0[3]) * (1200.0 / 600.0);
        assert!((rendered_aspect_2 - image_aspect).abs() < 0.01);
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
    fn transform_at_zoom_2() {
        let mut view = ViewState::new();
        // Square image in square window: both scales should be zoom
        view.update_dimensions(800, 800, 800, 800);
        view.zoom = 2.0;
        let t = view.transform();
        assert!((t.col0[0] - 2.0).abs() < 0.01);
        assert!((t.col0[3] - 2.0).abs() < 0.01);
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

    #[test]
    fn actual_size_zoom_below_one_for_small_images() {
        let mut view = ViewState::new();
        // 200x200 image in 800x800 window: actual size is 0.25
        view.update_dimensions(200, 200, 800, 800);
        let z = view.actual_size_zoom();
        assert!((z - 0.25).abs() < 0.01);
    }

    #[test]
    fn min_zoom_allows_small_image_actual_size() {
        let mut view = ViewState::new();
        view.update_dimensions(200, 200, 800, 800);
        let z = view.actual_size_zoom();
        view.set_min_zoom(z);
        view.actual_size();
        assert!((view.zoom - 0.25).abs() < 0.01);
    }

    #[test]
    fn zoom_out_stops_at_min_zoom() {
        let mut view = ViewState::new();
        view.update_dimensions(200, 200, 800, 800);
        view.set_min_zoom(view.actual_size_zoom());
        view.actual_size();
        // Zoom out a lot — should not go below min_zoom
        for _ in 0..200 {
            view.keyboard_zoom(false);
        }
        assert!((view.zoom - view.actual_size_zoom()).abs() < f32::EPSILON);
    }

    #[test]
    fn set_min_zoom_reclamps_current_zoom() {
        let mut view = ViewState::new();
        view.update_dimensions(200, 200, 800, 800);
        view.set_min_zoom(0.25);
        view.zoom = 0.1; // artificially low
        view.set_min_zoom(0.25);
        assert!((view.zoom - 0.25).abs() < f32::EPSILON);
    }
}
