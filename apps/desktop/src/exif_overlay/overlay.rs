//! Build the visual layers for the EXIF info overlay.
//!
//! Layout: a single rounded backdrop pill with rows of `label  value` text.
//! Rows are grouped into sections (camera, exposure, lens, date, image,
//! other), with a small vertical gap between sections instead of a
//! separator rule — quieter visually, and the gap alone is enough cue.
//!
//! Width matches the histogram so the two panels stack as one column. Panel
//! height is computed from the row count so an iPhone shot (no lens model,
//! no GPS) takes less space than a DSLR shot with everything filled in.
//!
//! Hidden entirely when `metadata` is empty — `parse_exif_metadata` already
//! returns `None` in that case, so reaching `build` means there's at least
//! one row to render.

use crate::decoding::ExifMetadata;
use crate::histogram::overlay::PANEL_HEIGHT as HISTOGRAM_PANEL_HEIGHT;
use crate::pixels::Logical;
use crate::render::overlay_style::{
    BACKDROP_COLOR, INTER_PANEL_MARGIN, PANEL_MARGIN_RIGHT, PANEL_RADIUS, PANEL_WIDTH,
};
use crate::render::text::{StandalonePill, TextBlock};

/// Output of `build`, fed into the same overlay-pool slots the histogram
/// already uses. No draw call here — the EXIF panel is text + pill only.
pub struct ExifOverlayBuild {
    pub text_blocks: Vec<TextBlock>,
    pub pills: Vec<StandalonePill>,
}

const PAD_X: f32 = 10.0;
const PAD_TOP: f32 = 8.0;
const PAD_BOTTOM: f32 = 9.0;
const ROW_HEIGHT: f32 = 14.0;
const SECTION_GAP: f32 = 5.0;
const FONT_SIZE: f32 = 11.0;
const LABEL_COLUMN_WIDTH: f32 = 78.0;

const TEXT_COLOR_VALUE: [u8; 4] = [255, 255, 255, 230];
const TEXT_COLOR_LABEL: [u8; 4] = [255, 255, 255, 145];

/// Build the EXIF panel for the given image's metadata. `histogram_visible`
/// shifts the panel down by the histogram height + an inter-panel gap; when
/// the histogram is hidden, the EXIF panel takes the histogram's anchor.
pub fn build(
    metadata: &ExifMetadata,
    window_width: Logical<f32>,
    content_offset_y: Logical<f32>,
    histogram_visible: bool,
) -> ExifOverlayBuild {
    let rows = build_rows(metadata);
    if rows.is_empty() {
        return ExifOverlayBuild {
            text_blocks: Vec::new(),
            pills: Vec::new(),
        };
    }

    let panel_x = window_width.0 - PANEL_WIDTH - PANEL_MARGIN_RIGHT;
    let panel_y = if histogram_visible {
        content_offset_y.0 + PANEL_MARGIN_RIGHT + HISTOGRAM_PANEL_HEIGHT + INTER_PANEL_MARGIN
    } else {
        content_offset_y.0 + PANEL_MARGIN_RIGHT
    };

    let panel_height = panel_height_for(&rows);

    let pills = vec![StandalonePill {
        x: Logical(panel_x),
        y: Logical(panel_y),
        width: Logical(PANEL_WIDTH),
        height: Logical(panel_height),
        corner_radius: Logical(PANEL_RADIUS),
        color: BACKDROP_COLOR,
    }];

    let mut text_blocks = Vec::with_capacity(rows.len() * 2);
    let mut y = panel_y + PAD_TOP;
    let mut prev_section: Option<u8> = None;
    for row in &rows {
        if let Some(prev) = prev_section
            && prev != row.section
        {
            y += SECTION_GAP;
        }
        prev_section = Some(row.section);

        match &row.kind {
            RowKind::LabelValue { label, value } => {
                let mut label_block =
                    TextBlock::new(label.clone(), Logical(panel_x + PAD_X), Logical(y));
                label_block.font_size = FONT_SIZE;
                label_block.line_height = ROW_HEIGHT;
                label_block.color = TEXT_COLOR_LABEL;
                text_blocks.push(label_block);

                let mut value_block = TextBlock::new(
                    value.clone(),
                    Logical(panel_x + PAD_X + LABEL_COLUMN_WIDTH),
                    Logical(y),
                );
                value_block.font_size = FONT_SIZE;
                value_block.line_height = ROW_HEIGHT;
                value_block.color = TEXT_COLOR_VALUE;
                value_block.max_render_width =
                    Some(Logical(PANEL_WIDTH - 2.0 * PAD_X - LABEL_COLUMN_WIDTH));
                text_blocks.push(value_block);
            }
            RowKind::ValueOnly(value) => {
                let mut value_block =
                    TextBlock::new(value.clone(), Logical(panel_x + PAD_X), Logical(y));
                value_block.font_size = FONT_SIZE;
                value_block.line_height = ROW_HEIGHT;
                value_block.color = TEXT_COLOR_VALUE;
                value_block.max_render_width = Some(Logical(PANEL_WIDTH - 2.0 * PAD_X));
                text_blocks.push(value_block);
            }
        }
        y += ROW_HEIGHT;
    }

    ExifOverlayBuild { text_blocks, pills }
}

fn panel_height_for(rows: &[Row]) -> f32 {
    let mut total = PAD_TOP + PAD_BOTTOM + rows.len() as f32 * ROW_HEIGHT;
    let mut prev: Option<u8> = None;
    for row in rows {
        if let Some(p) = prev
            && p != row.section
        {
            total += SECTION_GAP;
        }
        prev = Some(row.section);
    }
    total
}

enum RowKind {
    /// A `label: value` row split into two columns.
    LabelValue { label: String, value: String },
    /// A heading-like value-only row spanning the panel width — used for
    /// the camera body line where "Make Model" reads as a single piece.
    ValueOnly(String),
}

struct Row {
    section: u8,
    kind: RowKind,
}

const SECTION_CAMERA: u8 = 0;
const SECTION_EXPOSURE: u8 = 1;
const SECTION_LENS: u8 = 2;
const SECTION_DATE: u8 = 3;
const SECTION_IMAGE: u8 = 4;
const SECTION_OTHER: u8 = 5;

fn build_rows(m: &ExifMetadata) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::with_capacity(10);

    // Camera body, single line (`Make Model` collapses repetitive prefixes
    // like "Canon Canon EOS R5"). Drawn full-width as a heading.
    if let Some(line) = camera_line(m) {
        rows.push(Row {
            section: SECTION_CAMERA,
            kind: RowKind::ValueOnly(line),
        });
    }

    // Exposure triplet: shutter, aperture, ISO on one row when at least
    // two are present — that's how cameras print it. Otherwise show
    // whatever single item exists on its own row.
    if let Some(triplet) = exposure_triplet(m) {
        rows.push(Row {
            section: SECTION_EXPOSURE,
            kind: RowKind::LabelValue {
                label: "Exposure".to_string(),
                value: triplet,
            },
        });
    }
    if let Some(comp) = m.exposure_compensation {
        // Skip "0 EV" — showing it adds noise without information.
        if comp.abs() > 1e-3 {
            rows.push(Row {
                section: SECTION_EXPOSURE,
                kind: RowKind::LabelValue {
                    label: "EV".to_string(),
                    value: format_ev(comp),
                },
            });
        }
    }
    if let Some(program) = m.exposure_program.as_ref() {
        rows.push(Row {
            section: SECTION_EXPOSURE,
            kind: RowKind::LabelValue {
                label: "Mode".to_string(),
                value: program.clone(),
            },
        });
    }
    if let Some(metering) = m.metering_mode.as_ref() {
        rows.push(Row {
            section: SECTION_EXPOSURE,
            kind: RowKind::LabelValue {
                label: "Metering".to_string(),
                value: metering.clone(),
            },
        });
    }
    if let Some(flash) = m.flash.as_ref() {
        rows.push(Row {
            section: SECTION_EXPOSURE,
            kind: RowKind::LabelValue {
                label: "Flash".to_string(),
                value: flash.clone(),
            },
        });
    }
    if let Some(wb) = m.white_balance.as_ref() {
        rows.push(Row {
            section: SECTION_EXPOSURE,
            kind: RowKind::LabelValue {
                label: "WB".to_string(),
                value: wb.clone(),
            },
        });
    }

    // Lens, focal length.
    if let Some(lens) = m.lens_model.as_ref() {
        rows.push(Row {
            section: SECTION_LENS,
            kind: RowKind::LabelValue {
                label: "Lens".to_string(),
                value: lens.clone(),
            },
        });
    }
    if let Some(focal_line) = focal_length_line(m) {
        rows.push(Row {
            section: SECTION_LENS,
            kind: RowKind::LabelValue {
                label: "Focal".to_string(),
                value: focal_line,
            },
        });
    }

    // When taken.
    if let Some(date) = m.date_taken.as_ref() {
        rows.push(Row {
            section: SECTION_DATE,
            kind: RowKind::LabelValue {
                label: "Taken".to_string(),
                value: date.clone(),
            },
        });
    }

    // Pixel size.
    if let (Some(w), Some(h)) = (m.pixel_width, m.pixel_height) {
        rows.push(Row {
            section: SECTION_IMAGE,
            kind: RowKind::LabelValue {
                label: "Size".to_string(),
                value: format_pixel_size(w, h),
            },
        });
    }

    // Software, GPS.
    if let Some(software) = m.software.as_ref() {
        rows.push(Row {
            section: SECTION_OTHER,
            kind: RowKind::LabelValue {
                label: "Software".to_string(),
                value: software.clone(),
            },
        });
    }
    if let (Some(lat), Some(lon)) = (m.gps_latitude, m.gps_longitude) {
        rows.push(Row {
            section: SECTION_OTHER,
            kind: RowKind::LabelValue {
                label: "GPS".to_string(),
                value: format_gps(lat, lon),
            },
        });
    }
    if let Some(alt) = m.gps_altitude {
        rows.push(Row {
            section: SECTION_OTHER,
            kind: RowKind::LabelValue {
                label: "Altitude".to_string(),
                value: format!("{alt:.0} m"),
            },
        });
    }

    rows
}

fn camera_line(m: &ExifMetadata) -> Option<String> {
    match (m.camera_make.as_deref(), m.camera_model.as_deref()) {
        (Some(make), Some(model)) => {
            if model
                .to_ascii_lowercase()
                .starts_with(&make.to_ascii_lowercase())
            {
                Some(model.to_string())
            } else {
                Some(format!("{make} {model}"))
            }
        }
        (Some(make), None) => Some(make.to_string()),
        (None, Some(model)) => Some(model.to_string()),
        (None, None) => None,
    }
}

fn exposure_triplet(m: &ExifMetadata) -> Option<String> {
    let parts: Vec<String> = [
        m.exposure_time.clone(),
        m.f_number.map(|f| format!("f/{f:.1}")),
        m.iso.map(|iso| format!("ISO {iso}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("  "))
    }
}

fn focal_length_line(m: &ExifMetadata) -> Option<String> {
    match (m.focal_length_mm, m.focal_length_35mm) {
        (Some(actual), Some(eq)) => Some(format!("{actual:.0} mm  ({eq} mm eq)")),
        (Some(actual), None) => Some(format!("{actual:.0} mm")),
        (None, Some(eq)) => Some(format!("{eq} mm eq")),
        (None, None) => None,
    }
}

/// Format an EV compensation value with explicit sign. Callers must skip
/// near-zero values (`abs < 1e-3`) before calling — "0 EV" adds noise
/// without information, so we don't emit it from here.
fn format_ev(comp: f64) -> String {
    format!("{comp:+.1} EV")
}

fn format_pixel_size(w: u32, h: u32) -> String {
    let mp = (w as f64 * h as f64) / 1_000_000.0;
    if mp >= 1.0 {
        format!("{w} \u{00d7} {h}  ({mp:.1} MP)")
    } else {
        format!("{w} \u{00d7} {h}")
    }
}

fn format_gps(lat: f64, lon: f64) -> String {
    let lat_letter = if lat >= 0.0 { 'N' } else { 'S' };
    let lon_letter = if lon >= 0.0 { 'E' } else { 'W' };
    format!(
        "{lat_abs:.4}\u{00b0} {lat_letter}, {lon_abs:.4}\u{00b0} {lon_letter}",
        lat_abs = lat.abs(),
        lon_abs = lon.abs(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with(setup: impl FnOnce(&mut ExifMetadata)) -> ExifMetadata {
        let mut m = ExifMetadata::default();
        setup(&mut m);
        m
    }

    #[test]
    fn empty_metadata_yields_empty_build() {
        let build = build(
            &ExifMetadata::default(),
            Logical(1000.0),
            Logical(0.0),
            false,
        );
        assert!(build.text_blocks.is_empty());
        assert!(build.pills.is_empty());
    }

    #[test]
    fn camera_line_collapses_redundant_make_prefix() {
        let m = meta_with(|m| {
            m.camera_make = Some("Canon".into());
            m.camera_model = Some("Canon EOS R5".into());
        });
        assert_eq!(camera_line(&m).as_deref(), Some("Canon EOS R5"));
    }

    #[test]
    fn camera_line_combines_when_distinct() {
        let m = meta_with(|m| {
            m.camera_make = Some("SONY".into());
            m.camera_model = Some("ILCE-7M4".into());
        });
        assert_eq!(camera_line(&m).as_deref(), Some("SONY ILCE-7M4"));
    }

    #[test]
    fn exposure_triplet_format() {
        let m = meta_with(|m| {
            m.exposure_time = Some("1/250 s".into());
            m.f_number = Some(2.8);
            m.iso = Some(400);
        });
        assert_eq!(
            exposure_triplet(&m).as_deref(),
            Some("1/250 s  f/2.8  ISO 400")
        );
    }

    #[test]
    fn focal_length_with_35mm_equiv() {
        let m = meta_with(|m| {
            m.focal_length_mm = Some(50.0);
            m.focal_length_35mm = Some(75);
        });
        assert_eq!(focal_length_line(&m).as_deref(), Some("50 mm  (75 mm eq)"));
    }

    #[test]
    fn ev_formatter_signs_nonzero_values() {
        assert_eq!(format_ev(0.3), "+0.3 EV");
        assert_eq!(format_ev(-1.0), "-1.0 EV");
        assert_eq!(format_ev(2.7), "+2.7 EV");
    }

    #[test]
    fn full_build_produces_rows_with_label_and_value_blocks() {
        let m = meta_with(|m| {
            m.camera_make = Some("Canon".into());
            m.camera_model = Some("EOS R5".into());
            m.exposure_time = Some("1/250 s".into());
            m.f_number = Some(2.8);
            m.iso = Some(400);
            m.lens_model = Some("RF 50mm F1.2 L USM".into());
            m.focal_length_mm = Some(50.0);
            m.date_taken = Some("2024-08-15 12:34:56".into());
        });
        let build = build(&m, Logical(1200.0), Logical(0.0), true);
        assert!(build.text_blocks.len() >= 6);
        assert_eq!(build.pills.len(), 1);
    }
}
