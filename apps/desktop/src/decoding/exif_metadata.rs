//! EXIF metadata extraction for the EXIF info overlay.
//!
//! Pulls a curated subset of EXIF tags from JPEG / TIFF / WebP / HEIC files
//! via `nom-exif`, and from camera RAW files via rawler's already-parsed
//! `RawMetadata`. The two paths produce the same `ExifMetadata` shape, so
//! the overlay layer doesn't care which decoder produced it.
//!
//! Returns `None` when no EXIF segment is present, or when every field we
//! care about came back empty — the overlay then hides itself even if the
//! user toggled it on.
//!
//! Pre-formatted strings for the human-readable fields (shutter speed,
//! exposure program, metering mode, flash, white balance) live here so the
//! UI layer never needs to know the EXIF magic numbers. Numeric fields stay
//! raw (`f64` / `u32`) so the formatter can pick units at render time.

use std::io::Cursor;

use nom_exif::{EntryValue, ExifIter, ExifTag, MediaParser, MediaSource, URational};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExifMetadata {
    // Camera
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    // Lens
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub focal_length_35mm: Option<u32>,
    // Exposure
    pub exposure_time: Option<String>,
    pub f_number: Option<f64>,
    pub iso: Option<u32>,
    pub exposure_compensation: Option<f64>,
    pub exposure_program: Option<String>,
    pub metering_mode: Option<String>,
    pub flash: Option<String>,
    pub white_balance: Option<String>,
    // Image
    pub pixel_width: Option<u32>,
    pub pixel_height: Option<u32>,
    // Time
    pub date_taken: Option<String>,
    // GPS
    pub gps_latitude: Option<f64>,
    pub gps_longitude: Option<f64>,
    pub gps_altitude: Option<f64>,
    // Software
    pub software: Option<String>,
}

impl ExifMetadata {
    /// True when every field is `None`. Used to decide whether the overlay
    /// has anything to show.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Parse EXIF from raw file bytes. Returns `None` if the file has no EXIF
/// segment or every interesting field came back empty.
pub fn parse_exif_metadata(bytes: &[u8]) -> Option<ExifMetadata> {
    let mut parser = MediaParser::new();
    let cursor = Cursor::new(bytes);
    let ms = MediaSource::seekable(cursor).ok()?;
    // No `has_exif()` pre-check in nom-exif 3 — `parse_exif` reports a file without an EXIF
    // segment as an error, which is the same `None` for us.
    let iter: ExifIter = parser.parse_exif(ms).ok()?;
    let exif: nom_exif::Exif = iter.into();

    let lat_ref = exif.get(ExifTag::GPSLatitudeRef).and_then(text);
    let lon_ref = exif.get(ExifTag::GPSLongitudeRef).and_then(text);
    let gps_latitude = exif
        .get(ExifTag::GPSLatitude)
        .and_then(rational_array_to_dms)
        .map(|deg| apply_gps_ref(deg, lat_ref.as_deref(), b'S'));
    let gps_longitude = exif
        .get(ExifTag::GPSLongitude)
        .and_then(rational_array_to_dms)
        .map(|deg| apply_gps_ref(deg, lon_ref.as_deref(), b'W'));
    let gps_altitude = exif
        .get(ExifTag::GPSAltitude)
        .and_then(rational_to_f64)
        .map(|alt| {
            let neg = exif
                .get(ExifTag::GPSAltitudeRef)
                .and_then(value_to_u32)
                .is_some_and(|r| r == 1);
            if neg { -alt } else { alt }
        });

    let meta = ExifMetadata {
        camera_make: exif.get(ExifTag::Make).and_then(text),
        camera_model: exif.get(ExifTag::Model).and_then(text),
        lens_model: exif.get(ExifTag::LensModel).and_then(text),
        software: exif.get(ExifTag::Software).and_then(text),
        focal_length_mm: exif.get(ExifTag::FocalLength).and_then(rational_to_f64),
        focal_length_35mm: exif
            .get(ExifTag::FocalLengthIn35mmFilm)
            .and_then(value_to_u32),
        exposure_time: exif
            .get(ExifTag::ExposureTime)
            .and_then(rational_pair)
            .map(|(n, d)| format_exposure_time(n, d)),
        f_number: exif.get(ExifTag::FNumber).and_then(rational_to_f64),
        iso: exif.get(ExifTag::ISOSpeedRatings).and_then(value_to_u32),
        exposure_compensation: exif
            .get(ExifTag::ExposureBiasValue)
            .and_then(signed_rational_to_f64),
        exposure_program: exif
            .get(ExifTag::ExposureProgram)
            .and_then(value_to_u32)
            .and_then(format_exposure_program),
        metering_mode: exif
            .get(ExifTag::MeteringMode)
            .and_then(value_to_u32)
            .and_then(format_metering_mode),
        flash: exif
            .get(ExifTag::Flash)
            .and_then(value_to_u32)
            .map(format_flash),
        white_balance: exif
            .get(ExifTag::WhiteBalanceMode)
            .and_then(value_to_u32)
            .and_then(format_white_balance),
        pixel_width: exif
            .get(ExifTag::ExifImageWidth)
            .or_else(|| exif.get(ExifTag::ImageWidth))
            .and_then(value_to_u32),
        pixel_height: exif
            .get(ExifTag::ExifImageHeight)
            .or_else(|| exif.get(ExifTag::ImageHeight))
            .and_then(value_to_u32),
        date_taken: exif
            .get(ExifTag::DateTimeOriginal)
            .or_else(|| exif.get(ExifTag::CreateDate))
            .or_else(|| exif.get(ExifTag::ModifyDate))
            .and_then(date_text),
        gps_latitude,
        gps_longitude,
        gps_altitude,
    };

    if meta.is_empty() { None } else { Some(meta) }
}

/// Build an `ExifMetadata` from rawler's already-parsed `RawMetadata`. Reuses
/// the rawler fields rather than re-parsing the file with nom-exif: rawler
/// has already walked every IFD, applied per-camera quirks, and merged in
/// `LensDescription`, so this gives more complete data with less code.
pub fn parse_raw_exif(metadata: &rawler::decoders::RawMetadata) -> Option<ExifMetadata> {
    let exif = &metadata.exif;
    let mut meta = ExifMetadata {
        camera_make: nonempty(metadata.make.clone()),
        camera_model: nonempty(metadata.model.clone()),
        lens_model: exif.lens_model.clone().and_then(nonempty),
        software: None, // rawler doesn't surface it on `Exif`
        focal_length_mm: exif.focal_length.map(rawler_rational_to_f64),
        focal_length_35mm: None, // not on rawler::Exif
        exposure_time: exif
            .exposure_time
            .map(|r| format_exposure_time(r.n, r.d.max(1))),
        f_number: exif.fnumber.map(rawler_rational_to_f64),
        iso: exif.iso_speed_ratings.map(|v| v as u32).or(exif.iso_speed),
        exposure_compensation: exif.exposure_bias.map(|r| r.n as f64 / r.d.max(1) as f64),
        exposure_program: exif
            .exposure_program
            .map(|v| v as u32)
            .and_then(format_exposure_program),
        metering_mode: exif
            .metering_mode
            .map(|v| v as u32)
            .and_then(format_metering_mode),
        flash: exif.flash.map(|v| format_flash(v as u32)),
        white_balance: exif
            .white_balance
            .map(|v| v as u32)
            .and_then(format_white_balance),
        pixel_width: None,
        pixel_height: None,
        date_taken: exif
            .date_time_original
            .clone()
            .or_else(|| exif.create_date.clone())
            .or_else(|| exif.modify_date.clone())
            .map(normalize_exif_date),
        gps_latitude: None,
        gps_longitude: None,
        gps_altitude: None,
    };

    if let Some(gps) = &exif.gps {
        if let Some(arr) = gps.gps_latitude {
            let dms = [
                rawler_rational_to_f64(arr[0]),
                rawler_rational_to_f64(arr[1]),
                rawler_rational_to_f64(arr[2]),
            ];
            let deg = dms[0] + dms[1] / 60.0 + dms[2] / 3600.0;
            meta.gps_latitude = Some(apply_gps_ref(deg, gps.gps_latitude_ref.as_deref(), b'S'));
        }
        if let Some(arr) = gps.gps_longitude {
            let dms = [
                rawler_rational_to_f64(arr[0]),
                rawler_rational_to_f64(arr[1]),
                rawler_rational_to_f64(arr[2]),
            ];
            let deg = dms[0] + dms[1] / 60.0 + dms[2] / 3600.0;
            meta.gps_longitude = Some(apply_gps_ref(deg, gps.gps_longitude_ref.as_deref(), b'W'));
        }
        if let Some(alt) = gps.gps_altitude {
            let v = rawler_rational_to_f64(alt);
            let neg = gps.gps_altitude_ref == Some(1);
            meta.gps_altitude = Some(if neg { -v } else { v });
        }
    }

    if meta.is_empty() { None } else { Some(meta) }
}

// ─── Value extractors ───────────────────────────────────────────────────────

fn text(v: &EntryValue) -> Option<String> {
    if let EntryValue::Text(s) = v {
        nonempty(s.trim().to_string())
    } else {
        None
    }
}

fn nonempty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// Extract an EXIF date/time tag as a normalized `YYYY-MM-DD HH:MM:SS`
/// string. `nom-exif` decodes `DateTimeOriginal` / `CreateDate` /
/// `ModifyDate` into typed `DateTime` / `NaiveDateTime` variants, never `Text`,
/// so `text()` would silently drop the date on every JPEG / HEIC / WebP.
/// We handle both typed variants (dropping any timezone offset to show the
/// camera's wall-clock time) and keep a `Text` fallback for the rare
/// encoder that writes the date as a raw string.
fn date_text(v: &EntryValue) -> Option<String> {
    match v {
        EntryValue::DateTime(dt) => Some(dt.naive_local().format("%Y-%m-%d %H:%M:%S").to_string()),
        EntryValue::NaiveDateTime(dt) => Some(dt.format("%Y-%m-%d %H:%M:%S").to_string()),
        EntryValue::Text(s) => nonempty(s.trim().to_string()).map(normalize_exif_date),
        _ => None,
    }
}

fn value_to_u32(v: &EntryValue) -> Option<u32> {
    match v {
        EntryValue::U8(n) => Some(*n as u32),
        EntryValue::U16(n) => Some(*n as u32),
        EntryValue::U32(n) => Some(*n),
        EntryValue::U64(n) => u32::try_from(*n).ok(),
        EntryValue::I16(n) if *n >= 0 => Some(*n as u32),
        EntryValue::I32(n) if *n >= 0 => Some(*n as u32),
        // Some encoders write ISO as a `Text` integer string.
        EntryValue::Text(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn rational_to_f64(v: &EntryValue) -> Option<f64> {
    match v {
        EntryValue::URational(r) => Some(rational_pair_to_f64(r.numerator(), r.denominator())),
        EntryValue::URationalArray(arr) if !arr.is_empty() => {
            Some(rational_pair_to_f64(arr[0].numerator(), arr[0].denominator()))
        }
        EntryValue::F32(n) => Some(*n as f64),
        EntryValue::F64(n) => Some(*n),
        _ => None,
    }
}

fn signed_rational_to_f64(v: &EntryValue) -> Option<f64> {
    match v {
        EntryValue::IRational(r) => r.to_f64(),
        EntryValue::URational(r) => Some(rational_pair_to_f64(r.numerator(), r.denominator())),
        EntryValue::F32(n) => Some(*n as f64),
        EntryValue::F64(n) => Some(*n),
        _ => None,
    }
}

fn rational_pair(v: &EntryValue) -> Option<(u32, u32)> {
    match v {
        EntryValue::URational(r) => Some((r.numerator(), r.denominator().max(1))),
        EntryValue::URationalArray(arr) if !arr.is_empty() => Some((arr[0].numerator(), arr[0].denominator().max(1))),
        _ => None,
    }
}

fn rational_array_to_dms(v: &EntryValue) -> Option<f64> {
    let arr: &[URational] = match v {
        EntryValue::URationalArray(a) if a.len() >= 3 => a,
        _ => return None,
    };
    let d = rational_pair_to_f64(arr[0].numerator(), arr[0].denominator());
    let m = rational_pair_to_f64(arr[1].numerator(), arr[1].denominator());
    let s = rational_pair_to_f64(arr[2].numerator(), arr[2].denominator());
    Some(d + m / 60.0 + s / 3600.0)
}

fn rational_pair_to_f64(n: u32, d: u32) -> f64 {
    let denom = if d == 0 { 1 } else { d };
    n as f64 / denom as f64
}

fn rawler_rational_to_f64(r: rawler::formats::tiff::Rational) -> f64 {
    rational_pair_to_f64(r.n, r.d)
}

fn apply_gps_ref(degrees: f64, reference: Option<&str>, negative_letter: u8) -> f64 {
    match reference {
        Some(r) if r.trim().bytes().next() == Some(negative_letter) => -degrees.abs(),
        _ => degrees,
    }
}

// ─── Pretty printers ────────────────────────────────────────────────────────

/// Format a shutter time. Sub-second exposures render as `1/250 s`; longer
/// ones as `2.5"`. The seconds glyph (`"`) matches camera firmware UIs.
pub fn format_exposure_time(numerator: u32, denominator: u32) -> String {
    let n = numerator;
    let d = denominator.max(1);
    let secs = n as f64 / d as f64;
    if secs >= 1.0 {
        if secs.fract() < 0.05 {
            format!("{}\"", secs.round() as u32)
        } else {
            format!("{secs:.1}\"")
        }
    } else if n == 0 {
        "0".to_string()
    } else if n == 1 {
        format!("1/{d} s")
    } else {
        // Reduce N/D to 1/X when possible so `10/2500` shows as `1/250 s`.
        let reduced = d / n.max(1);
        format!("1/{reduced} s")
    }
}

fn format_exposure_program(code: u32) -> Option<String> {
    let s = match code {
        1 => "Manual",
        2 => "Program AE",
        3 => "Aperture priority",
        4 => "Shutter priority",
        5 => "Creative (slow)",
        6 => "Action (fast)",
        7 => "Portrait",
        8 => "Landscape",
        9 => "Bulb",
        // 0 ("not defined") and unknown codes get hidden rather than
        // shown as a meaningless number.
        _ => return None,
    };
    Some(s.to_string())
}

fn format_metering_mode(code: u32) -> Option<String> {
    let s = match code {
        1 => "Average",
        2 => "Center-weighted",
        3 => "Spot",
        4 => "Multi-spot",
        5 => "Pattern",
        6 => "Partial",
        255 => "Other",
        _ => return None,
    };
    Some(s.to_string())
}

fn format_flash(code: u32) -> String {
    let fired = code & 0x01 != 0;
    let no_flash_function = code & 0x20 != 0;
    if no_flash_function {
        "No flash".to_string()
    } else if fired {
        "Fired".to_string()
    } else {
        "Did not fire".to_string()
    }
}

fn format_white_balance(code: u32) -> Option<String> {
    match code {
        0 => Some("Auto".to_string()),
        1 => Some("Manual".to_string()),
        _ => None,
    }
}

/// Convert the EXIF date format `"YYYY:MM:DD HH:MM:SS"` to ISO-ish
/// `"YYYY-MM-DD HH:MM:SS"`. Leaves anything that doesn't match the
/// 19-char EXIF shape untouched.
fn normalize_exif_date(s: String) -> String {
    let bytes = s.as_bytes();
    if bytes.len() < 10 || bytes[4] != b':' || bytes[7] != b':' {
        return s;
    }
    let mut out = s.into_bytes();
    out[4] = b'-';
    out[7] = b'-';
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutter_below_one_second() {
        assert_eq!(format_exposure_time(1, 250), "1/250 s");
        assert_eq!(format_exposure_time(10, 2500), "1/250 s");
    }

    #[test]
    fn shutter_one_second_and_above() {
        assert_eq!(format_exposure_time(1, 1), "1\"");
        assert_eq!(format_exposure_time(5, 2), "2.5\"");
        assert_eq!(format_exposure_time(30, 1), "30\"");
    }

    #[test]
    fn date_text_from_naive_datetime() {
        // `nom-exif` decodes a date tag with no offset into `NaiveDateTime`.
        use chrono::NaiveDate;
        let dt = NaiveDate::from_ymd_opt(2024, 8, 15)
            .unwrap()
            .and_hms_opt(12, 34, 56)
            .unwrap();
        assert_eq!(
            date_text(&EntryValue::NaiveDateTime(dt)).as_deref(),
            Some("2024-08-15 12:34:56")
        );
    }

    #[test]
    fn date_text_from_offset_datetime_strips_offset() {
        // With an OffsetTime tag present, `nom-exif` decodes into `DateTime`. We
        // show the wall-clock time the camera recorded, dropping the offset.
        use chrono::{FixedOffset, TimeZone};
        let dt = FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2023, 7, 9, 20, 36, 33)
            .unwrap();
        assert_eq!(
            date_text(&EntryValue::DateTime(dt)).as_deref(),
            Some("2023-07-09 20:36:33")
        );
    }

    #[test]
    fn date_text_from_text_normalizes_colons() {
        // Rare, but some encoders write the date as a raw EXIF string.
        assert_eq!(
            date_text(&EntryValue::Text("2024:08:15 12:34:56".to_string())).as_deref(),
            Some("2024-08-15 12:34:56")
        );
    }

    #[test]
    fn date_normalises_colons_to_dashes() {
        assert_eq!(
            normalize_exif_date("2024:08:15 12:34:56".to_string()),
            "2024-08-15 12:34:56"
        );
    }

    #[test]
    fn date_passes_through_unknown_format() {
        assert_eq!(
            normalize_exif_date("2024-08-15T12:34:56Z".to_string()),
            "2024-08-15T12:34:56Z"
        );
    }

    #[test]
    fn metadata_round_trip_via_little_exif() {
        // Build a 4×4 RGB JPEG in memory, inject EXIF via `little_exif`,
        // and parse it back. Verifies the wiring end-to-end without
        // checking in a binary fixture.
        use little_exif::exif_tag::ExifTag as LeTag;
        use little_exif::filetype::FileExtension;
        use little_exif::metadata::Metadata;
        use little_exif::rational::uR64;

        let mut img = image::RgbImage::new(4, 4);
        for px in img.pixels_mut() {
            *px = image::Rgb([200, 100, 50]);
        }
        let mut buf: Vec<u8> = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .expect("encode JPEG");

        let mut md = Metadata::new();
        md.set_tag(LeTag::Make("TestMake".into()));
        md.set_tag(LeTag::Model("TestModel".into()));
        md.set_tag(LeTag::LensModel("TestLens 50mm".into()));
        md.set_tag(LeTag::Software("PrvwTest 1.0".into()));
        md.set_tag(LeTag::FNumber(vec![uR64 {
            nominator: 28,
            denominator: 10,
        }]));
        md.set_tag(LeTag::ExposureTime(vec![uR64 {
            nominator: 1,
            denominator: 250,
        }]));
        md.set_tag(LeTag::ISO(vec![400]));
        md.set_tag(LeTag::FocalLength(vec![uR64 {
            nominator: 50,
            denominator: 1,
        }]));
        md.set_tag(LeTag::DateTimeOriginal("2024:08:15 12:34:56".into()));

        md.write_to_vec(&mut buf, FileExtension::JPEG)
            .expect("inject EXIF");

        let parsed = parse_exif_metadata(&buf).expect("EXIF should parse");
        assert_eq!(parsed.camera_make.as_deref(), Some("TestMake"));
        assert_eq!(parsed.camera_model.as_deref(), Some("TestModel"));
        assert_eq!(parsed.lens_model.as_deref(), Some("TestLens 50mm"));
        assert_eq!(parsed.software.as_deref(), Some("PrvwTest 1.0"));
        assert!((parsed.f_number.unwrap() - 2.8).abs() < 0.01);
        assert_eq!(parsed.exposure_time.as_deref(), Some("1/250 s"));
        assert_eq!(parsed.iso, Some(400));
        assert!((parsed.focal_length_mm.unwrap() - 50.0).abs() < 0.01);
        // The date must come through. `nom-exif` decodes `DateTimeOriginal`
        // into a typed `NaiveDateTime`, not `Text`, so this guards against
        // the regression where the extractor only matched `Text` and
        // silently dropped every JPEG/HEIC date.
        assert_eq!(parsed.date_taken.as_deref(), Some("2024-08-15 12:34:56"));
    }

    #[test]
    fn missing_exif_returns_none() {
        // A bare 4×4 PNG has no EXIF segment; parse should return None.
        let mut img = image::RgbImage::new(4, 4);
        for px in img.pixels_mut() {
            *px = image::Rgb([10, 20, 30]);
        }
        let mut buf: Vec<u8> = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        assert!(parse_exif_metadata(&buf).is_none());
    }
}
