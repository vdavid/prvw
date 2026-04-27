//! Histogram overlay: 256-bin RGB plot anchored to the top-right of the
//! window. Toggled by View → Histogram or the bare H key. Computed once per
//! image, cached on the `histogram::State` struct.
//!
//! - `compute` — pixel-buffer scan that produces `HistogramData`.
//! - `overlay` — visual layer: backdrop pill + bar plot + hover readout.

pub mod compute;
pub mod overlay;

use crate::pixels::Logical;
use crate::settings::Settings;

pub use compute::HistogramData;

/// Cached rect of where the histogram was last drawn, in logical points.
/// The cursor-moved handler reads this to map mouse position to a bin.
#[derive(Clone, Copy, Debug)]
pub struct HistogramRect {
    pub x: Logical<f32>,
    pub y: Logical<f32>,
    pub width: Logical<f32>,
    pub height: Logical<f32>,
}

impl HistogramRect {
    /// Map a cursor position to a bin (0..=255), or `None` if the position
    /// is outside the rect.
    pub fn bin_at(&self, cx: Logical<f32>, cy: Logical<f32>) -> Option<u8> {
        if cx.0 < self.x.0
            || cx.0 >= self.x.0 + self.width.0
            || cy.0 < self.y.0
            || cy.0 >= self.y.0 + self.height.0
        {
            return None;
        }
        let frac = ((cx.0 - self.x.0) / self.width.0).clamp(0.0, 0.999_999);
        Some((frac * 256.0) as u8)
    }
}

pub struct State {
    pub visible: bool,
    pub data: Option<HistogramData>,
    pub hover_bin: Option<u8>,
    pub last_rect: Option<HistogramRect>,
}

impl State {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            visible: settings.histogram_visible,
            data: None,
            hover_bin: None,
            last_rect: None,
        }
    }
}
