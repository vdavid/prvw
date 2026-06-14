//! Shared style constants for top-right overlay panels (histogram, EXIF
//! info). Both panels stack with the same width, the same right margin, the
//! same backdrop color, and the same corner radius so they read as one
//! visual system rather than two stand-alone widgets.

/// Panel width in logical pixels. Histogram and EXIF use the same value so
/// they stack cleanly when both are visible.
pub const PANEL_WIDTH: f32 = 256.0;

/// Corner radius of the rounded backdrop pill, in logical pixels.
pub const PANEL_RADIUS: f32 = 7.0;

/// Right-edge margin from the window edge, in logical pixels. Doubles as
/// the top margin from `content_offset_y()` so the first panel hugs the
/// window corner symmetrically.
pub const PANEL_MARGIN_RIGHT: f32 = 7.0;

/// Backdrop fill color (translucent black) for non-text panels.
pub const BACKDROP_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.82];

/// Vertical gap between the histogram panel and the EXIF panel when both
/// are visible, in logical pixels.
pub const INTER_PANEL_MARGIN: f32 = 8.0;
