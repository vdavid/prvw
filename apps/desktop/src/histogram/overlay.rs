//! Build the visual layers for the histogram overlay.
//!
//! Returns `(text_blocks, standalone_pills, draw_call)`. The renderer composes
//! them in this order: backdrop pill → axis tick pills → histogram bars →
//! text labels and hover readout.

use super::{HistogramData, HistogramRect};
use crate::pixels::Logical;
use crate::render::renderer::HistogramDrawCall;
use crate::render::text::{StandalonePill, TextBlock};

/// Output of `build`, fed straight into `Renderer::render`.
pub struct HistogramOverlayBuild<'a> {
    pub text_blocks: Vec<TextBlock>,
    pub pills: Vec<StandalonePill>,
    pub draw_call: HistogramDrawCall<'a>,
    /// The plot rect that bins are mapped from. Cached on `histogram::State`
    /// so the cursor-moved handler can map mouse positions to bins without
    /// recomputing layout.
    pub plot_rect: HistogramRect,
}

const PANEL_WIDTH: f32 = 256.0;
const PANEL_HEIGHT: f32 = 110.0;
const PANEL_MARGIN_RIGHT: f32 = 7.0;
const PANEL_PAD_X: f32 = 10.0;
const PANEL_PAD_TOP: f32 = 22.0;
const PANEL_PAD_BOTTOM: f32 = 18.0;
const PANEL_RADIUS: f32 = 7.0;
const BACKDROP_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.55];
const TICK_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.10];

/// Build the histogram overlay for the given window width and content offset.
pub fn build<'a>(
    data: &'a HistogramData,
    hover_bin: Option<u8>,
    window_width: Logical<f32>,
    content_offset_y: Logical<f32>,
) -> HistogramOverlayBuild<'a> {
    let panel_x = window_width.0 - PANEL_WIDTH - PANEL_MARGIN_RIGHT;
    let panel_y = content_offset_y.0 + PANEL_MARGIN_RIGHT;

    let mut pills = Vec::with_capacity(8);

    // Backdrop pill.
    pills.push(StandalonePill {
        x: Logical(panel_x),
        y: Logical(panel_y),
        width: Logical(PANEL_WIDTH),
        height: Logical(PANEL_HEIGHT),
        corner_radius: Logical(PANEL_RADIUS),
        color: BACKDROP_COLOR,
    });

    // Plot rect — where the bars actually live, inside the padded area.
    let plot_x = panel_x + PANEL_PAD_X;
    let plot_y = panel_y + PANEL_PAD_TOP;
    let plot_w = PANEL_WIDTH - 2.0 * PANEL_PAD_X;
    let plot_h = PANEL_HEIGHT - PANEL_PAD_TOP - PANEL_PAD_BOTTOM;

    // Axis tick marks at 0, 64, 128, 192, 255. Drawn as 1px-wide pills behind
    // the bars at low alpha so they sit underneath without competing.
    for &bin in &[0u32, 64, 128, 192, 255] {
        let frac = bin as f32 / 255.0;
        let tick_x = plot_x + frac * plot_w - 0.5;
        pills.push(StandalonePill {
            x: Logical(tick_x),
            y: Logical(plot_y),
            width: Logical(1.0),
            height: Logical(plot_h),
            corner_radius: Logical(0.0),
            color: TICK_COLOR,
        });
    }

    let plot_rect = HistogramRect {
        x: Logical(plot_x),
        y: Logical(plot_y),
        width: Logical(plot_w),
        height: Logical(plot_h),
    };

    let draw_call = HistogramDrawCall {
        rect: StandalonePill {
            x: Logical(plot_x),
            y: Logical(plot_y),
            width: Logical(plot_w),
            height: Logical(plot_h),
            corner_radius: Logical(0.0),
            color: [0.0; 4],
        },
        data,
    };

    // Title above the plot, plus the hover readout when a bin is hovered.
    let mut text_blocks: Vec<TextBlock> = Vec::with_capacity(2);
    let title_text = match hover_bin {
        Some(bin) => format!(
            "bin {bin}  R {}  G {}  B {}",
            format_count(data.r[bin as usize]),
            format_count(data.g[bin as usize]),
            format_count(data.b[bin as usize]),
        ),
        None => "Histogram".to_string(),
    };
    let mut title = TextBlock::new(
        title_text,
        Logical(panel_x + PANEL_PAD_X),
        Logical(panel_y + 4.0),
    );
    title.font_size = 11.0;
    title.line_height = 14.0;
    title.color = [255, 255, 255, 220];
    text_blocks.push(title);

    HistogramOverlayBuild {
        text_blocks,
        pills,
        draw_call,
        plot_rect,
    }
}

/// Format a bin count compactly. `12,345` renders as `12.3k`.
fn format_count(n: u32) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f32 / 1_000.0)
    } else {
        format!("{:.1}M", n as f32 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_formatter() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1.0k");
        assert_eq!(format_count(12_345), "12.3k");
        assert_eq!(format_count(2_500_000), "2.5M");
    }
}
