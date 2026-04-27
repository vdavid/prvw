//! Build the visual layers for the EXIF info overlay.
//!
//! Layout: a single rounded backdrop pill with a small "Exif info" title at
//! the top, then rows of `label  value` text on a fixed line pitch. Section
//! grouping (camera, exposure, lens, date, image, other) is implicit in the
//! label names — every row uses the same vertical pitch so the rhythm stays
//! even regardless of how many sections an image actually fills.
//!
//! Width matches the histogram so the two panels stack as one column. Panel
//! height is computed from the row count, with extra slots reserved for any
//! row whose value wraps to multiple lines (long Software strings are the
//! common case).
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
use crate::render::text::{StandalonePill, TextBlock, count_wrapped_lines};

/// Output of `build`, fed into the same overlay-pool slots the histogram
/// already uses. No draw call here — the EXIF panel is text + pill only.
pub struct ExifOverlayBuild {
    pub text_blocks: Vec<TextBlock>,
    pub pills: Vec<StandalonePill>,
}

const PAD_X: f32 = 10.0;
const PAD_TOP: f32 = 4.0;
const PAD_BOTTOM: f32 = 9.0;
/// Single line pitch for every row (title and data). Keeping every row on
/// the same baseline pitch is what gives the panel its even rhythm.
const LINE_PITCH: f32 = 14.0;
const FONT_SIZE: f32 = 11.0;
const LABEL_COLUMN_WIDTH: f32 = 78.0;

const TEXT_COLOR_TITLE: [u8; 4] = [255, 255, 255, 220];
const TEXT_COLOR_VALUE: [u8; 4] = [255, 255, 255, 230];
const TEXT_COLOR_LABEL: [u8; 4] = [255, 255, 255, 145];

const TITLE_TEXT: &str = "Exif info";

/// Width available to a row's value column, in logical points. Used both
/// for `TextBlock.max_render_width` (so glyphon wraps long strings) and for
/// the wrap-line count that grows the panel height to match.
fn value_column_width() -> f32 {
    PANEL_WIDTH - 2.0 * PAD_X - LABEL_COLUMN_WIDTH
}

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

    // Measure each row's wrapped line count once, up front. Reused for both
    // the panel-height computation and the per-row vertical advance.
    let value_width = value_column_width();
    let row_lines: Vec<usize> = rows
        .iter()
        .map(|row| count_wrapped_lines(&row.value, FONT_SIZE, LINE_PITCH, value_width).max(1))
        .collect();
    let total_data_lines: usize = row_lines.iter().sum();

    let panel_x = window_width.0 - PANEL_WIDTH - PANEL_MARGIN_RIGHT;
    let panel_y = if histogram_visible {
        content_offset_y.0 + PANEL_MARGIN_RIGHT + HISTOGRAM_PANEL_HEIGHT + INTER_PANEL_MARGIN
    } else {
        content_offset_y.0 + PANEL_MARGIN_RIGHT
    };

    let panel_height = panel_height_for(total_data_lines);

    let pills = vec![StandalonePill {
        x: Logical(panel_x),
        y: Logical(panel_y),
        width: Logical(PANEL_WIDTH),
        height: Logical(panel_height),
        corner_radius: Logical(PANEL_RADIUS),
        color: BACKDROP_COLOR,
    }];

    let mut text_blocks = Vec::with_capacity(rows.len() * 2 + 1);

    // Title row — same pitch as data rows so the rhythm stays even.
    let mut title = TextBlock::new(
        TITLE_TEXT.to_string(),
        Logical(panel_x + PAD_X),
        Logical(panel_y + PAD_TOP),
    );
    title.font_size = FONT_SIZE;
    title.line_height = LINE_PITCH;
    title.color = TEXT_COLOR_TITLE;
    text_blocks.push(title);

    let mut y = panel_y + PAD_TOP + LINE_PITCH;
    for (row, &lines) in rows.iter().zip(row_lines.iter()) {
        let mut label_block =
            TextBlock::new(row.label.clone(), Logical(panel_x + PAD_X), Logical(y));
        label_block.font_size = FONT_SIZE;
        label_block.line_height = LINE_PITCH;
        label_block.color = TEXT_COLOR_LABEL;
        text_blocks.push(label_block);

        let mut value_block = TextBlock::new(
            row.value.clone(),
            Logical(panel_x + PAD_X + LABEL_COLUMN_WIDTH),
            Logical(y),
        );
        value_block.font_size = FONT_SIZE;
        value_block.line_height = LINE_PITCH;
        value_block.color = TEXT_COLOR_VALUE;
        value_block.max_render_width = Some(Logical(value_width));
        text_blocks.push(value_block);

        y += LINE_PITCH * lines as f32;
    }

    ExifOverlayBuild { text_blocks, pills }
}

/// Panel height = top padding + title row + every data line at one pitch +
/// bottom padding. `total_data_lines` already includes wrap-lines, so a
/// row whose value wraps to two lines contributes 2 to the sum.
fn panel_height_for(total_data_lines: usize) -> f32 {
    PAD_TOP + LINE_PITCH + total_data_lines as f32 * LINE_PITCH + PAD_BOTTOM
}

struct Row {
    label: String,
    value: String,
}

fn build_rows(m: &ExifMetadata) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::with_capacity(10);

    // Camera body, now a label-value row instead of a heading. `Make Model`
    // collapses repetitive prefixes like "Canon Canon EOS R5".
    if let Some(line) = camera_line(m) {
        rows.push(Row {
            label: "Camera".to_string(),
            value: line,
        });
    }

    // Exposure triplet: shutter, aperture, ISO on one row when at least
    // two are present — that's how cameras print it. Otherwise show
    // whatever single item exists on its own row.
    if let Some(triplet) = exposure_triplet(m) {
        rows.push(Row {
            label: "Exposure".to_string(),
            value: triplet,
        });
    }
    if let Some(comp) = m.exposure_compensation {
        // Skip "0 EV" — showing it adds noise without information.
        if comp.abs() > 1e-3 {
            rows.push(Row {
                label: "EV".to_string(),
                value: format_ev(comp),
            });
        }
    }
    if let Some(program) = m.exposure_program.as_ref() {
        rows.push(Row {
            label: "Mode".to_string(),
            value: program.clone(),
        });
    }
    if let Some(metering) = m.metering_mode.as_ref() {
        rows.push(Row {
            label: "Metering".to_string(),
            value: metering.clone(),
        });
    }
    if let Some(flash) = m.flash.as_ref() {
        rows.push(Row {
            label: "Flash".to_string(),
            value: flash.clone(),
        });
    }
    if let Some(wb) = m.white_balance.as_ref() {
        rows.push(Row {
            label: "WB".to_string(),
            value: wb.clone(),
        });
    }

    // Lens, focal length.
    if let Some(lens) = m.lens_model.as_ref() {
        rows.push(Row {
            label: "Lens".to_string(),
            value: lens.clone(),
        });
    }
    if let Some(focal_line) = focal_length_line(m) {
        rows.push(Row {
            label: "Focal".to_string(),
            value: focal_line,
        });
    }

    // When taken.
    if let Some(date) = m.date_taken.as_ref() {
        rows.push(Row {
            label: "Taken".to_string(),
            value: date.clone(),
        });
    }

    // Pixel size.
    if let (Some(w), Some(h)) = (m.pixel_width, m.pixel_height) {
        rows.push(Row {
            label: "Size".to_string(),
            value: format_pixel_size(w, h),
        });
    }

    // Software, GPS.
    if let Some(software) = m.software.as_ref() {
        rows.push(Row {
            label: "Software".to_string(),
            value: software.clone(),
        });
    }
    if let (Some(lat), Some(lon)) = (m.gps_latitude, m.gps_longitude) {
        rows.push(Row {
            label: "GPS".to_string(),
            value: format_gps(lat, lon),
        });
    }
    if let Some(alt) = m.gps_altitude {
        rows.push(Row {
            label: "Altitude".to_string(),
            value: format!("{alt:.0} m"),
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
        // Title + label/value pair per row.
        assert!(build.text_blocks.len() > 6);
        assert_eq!(build.pills.len(), 1);
    }

    #[test]
    fn title_text_is_exif_info() {
        let m = meta_with(|m| {
            m.camera_make = Some("Canon".into());
            m.camera_model = Some("EOS R5".into());
        });
        let build = build(&m, Logical(1200.0), Logical(0.0), false);
        assert_eq!(build.text_blocks[0].text, "Exif info");
    }

    #[test]
    fn camera_info_renders_as_first_data_row_labeled_camera() {
        let m = meta_with(|m| {
            m.camera_make = Some("FUJIFILM".into());
            m.camera_model = Some("FinePix S7000".into());
        });
        let build = build(&m, Logical(1200.0), Logical(0.0), false);
        // Index 0 = title, 1 = first label, 2 = first value.
        assert_eq!(build.text_blocks[1].text, "Camera");
        assert_eq!(build.text_blocks[2].text, "FUJIFILM FinePix S7000");
    }

    #[test]
    fn panel_height_uses_fixed_line_pitch_for_all_rows() {
        // Three rows that all fit on one line each → height matches the
        // closed-form `PAD_TOP + (1 + 3) * LINE_PITCH + PAD_BOTTOM`. No
        // section gaps, regardless of which sections the rows belong to.
        let m = meta_with(|m| {
            m.camera_make = Some("Canon".into());
            m.camera_model = Some("EOS R5".into());
            m.exposure_time = Some("1/250 s".into());
            m.lens_model = Some("RF 50mm F1.2".into());
        });
        let build = build(&m, Logical(1200.0), Logical(0.0), false);
        let expected = PAD_TOP + LINE_PITCH + 3.0 * LINE_PITCH + PAD_BOTTOM;
        assert!(
            (build.pills[0].height.0 - expected).abs() < 0.01,
            "expected panel height {expected}, got {}",
            build.pills[0].height.0
        );
    }

    #[test]
    fn panel_height_grows_for_wrapped_value() {
        // A long Software string forces wrapping; the panel height must
        // include the extra wrap-line at the same line pitch as a normal
        // row. We compute the expected line count via the same helper the
        // builder uses, so the assertion stays font-agnostic.
        let long_software = "Capture One 23 (16.3.0.1234) — Phase One A/S Build 9876543210 macOS \
            arm64 release channel"
            .to_string();
        let m = meta_with(|m| {
            m.camera_make = Some("Canon".into());
            m.camera_model = Some("EOS R5".into());
            m.software = Some(long_software.clone());
        });
        let build = build(&m, Logical(1200.0), Logical(0.0), false);

        let value_width = value_column_width();
        let software_lines =
            count_wrapped_lines(&long_software, FONT_SIZE, LINE_PITCH, value_width);
        assert!(
            software_lines >= 2,
            "test expected the long string to wrap, got {software_lines} line(s)"
        );

        // 2 rows total: Camera (1 line) + Software (`software_lines` lines).
        let expected_data_lines = 1 + software_lines;
        let expected = PAD_TOP + LINE_PITCH + expected_data_lines as f32 * LINE_PITCH + PAD_BOTTOM;
        assert!(
            (build.pills[0].height.0 - expected).abs() < 0.01,
            "expected panel height {expected} (= PAD_TOP + (1 + {expected_data_lines}) * LINE_PITCH + PAD_BOTTOM), got {}",
            build.pills[0].height.0
        );
    }
}
