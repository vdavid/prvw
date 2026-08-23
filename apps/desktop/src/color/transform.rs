use moxcms::{ColorProfile, InPlaceTransformExecutor, Layout, RenderingIntent, TransformOptions};
use std::sync::OnceLock;
use std::time::Instant;

/// Returns the sRGB ICC profile bytes, generated once by `moxcms` from the
/// spec's primaries and transfer curve.
///
/// Generated rather than read from the operating system: every platform keeps
/// its system sRGB profile somewhere different, and a Linux box often has none
/// at all. `moxcms` can build the profile itself, so there's nothing to
/// license, nothing to bundle, and nothing to keep in sync. Same argument as
/// [`crate::color::profiles`]'s linear Rec.2020 factory.
pub fn srgb_icc_bytes() -> &'static [u8] {
    static SRGB: OnceLock<Vec<u8>> = OnceLock::new();
    SRGB.get_or_init(|| {
        ColorProfile::new_srgb()
            .encode()
            .expect("moxcms' sRGB profile always encodes cleanly")
    })
}

/// Transform RGBA8 pixels from a source ICC profile to a target ICC profile, in-place.
/// Skips the transform when the profiles are byte-equal, and again when the
/// built transform turns out to be a no-op (see [`transform_is_negligible`]).
/// Silently returns on malformed profiles (the image displays as-is).
pub fn transform_icc(
    rgba: &mut [u8],
    source_icc: &[u8],
    target_icc: &[u8],
    use_relative_colorimetric: bool,
) {
    if target_icc.is_empty() {
        return; // ICC color management is disabled
    }
    if profiles_match(source_icc, target_icc) {
        log::debug!("Source and target ICC profiles match, skipping transform");
        return;
    }

    let source = match ColorProfile::new_from_slice(source_icc) {
        Ok(p) => p,
        Err(e) => {
            log::debug!("Skipping ICC transform: couldn't parse source profile ({e})");
            return;
        }
    };

    let target = match ColorProfile::new_from_slice(target_icc) {
        Ok(p) => p,
        Err(e) => {
            log::debug!("Skipping ICC transform: couldn't parse target profile ({e})");
            return;
        }
    };

    let intent = if use_relative_colorimetric {
        RenderingIntent::RelativeColorimetric
    } else {
        RenderingIntent::Perceptual
    };
    let options = TransformOptions {
        rendering_intent: intent,
        ..TransformOptions::default()
    };
    let transform: std::sync::Arc<dyn InPlaceTransformExecutor<u8> + Send + Sync> =
        match source.create_in_place_transform_8bit(Layout::Rgba, &target, options) {
            Ok(t) => t,
            Err(e) => {
                log::debug!("Skipping ICC transform: couldn't create transform ({e})");
                return;
            }
        };

    if transform_is_negligible(transform.as_ref()) {
        log::debug!("Source and target ICC profiles describe the same colors, skipping transform");
        return;
    }

    let start = Instant::now();
    if let Err(e) = transform.transform(rgba) {
        log::debug!("ICC transform failed: {e}");
        return;
    }
    let pixel_count = rgba.len() / 4;
    let source_desc = profile_description(&source);
    let target_desc = profile_description(&target);
    let intent_name = if use_relative_colorimetric {
        "relative"
    } else {
        "perceptual"
    };
    let pixel_count_fmt = format_with_separators(pixel_count);
    log::debug!(
        "ICC transform: {source_desc} -> {target_desc}, {intent_name} ({pixel_count_fmt} pixels) in {}ms",
        start.elapsed().as_millis()
    );
}

/// Transform f32 RGB pixels from a source `ColorProfile` into a target ICC
/// profile, in place. Input and output layouts are both `Rgb` (three
/// components, no alpha). Used by the RAW decode path to convert a linear
/// wide-gamut intermediate buffer into the display's color space without
/// going through an 8-bit round trip first — that would clip anything
/// outside [0, 1] before the transform ever sees it.
///
/// Silently skips the transform on parse/setup errors and leaves the buffer
/// untouched. The caller decides what to render in that case.
pub fn transform_f32_with_profile(
    rgb: &mut [f32],
    source: &ColorProfile,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
) {
    if target_icc.is_empty() {
        return;
    }

    let target = match ColorProfile::new_from_slice(target_icc) {
        Ok(p) => p,
        Err(e) => {
            log::debug!("Skipping f32 ICC transform: couldn't parse target profile ({e})");
            return;
        }
    };

    let intent = if use_relative_colorimetric {
        RenderingIntent::RelativeColorimetric
    } else {
        RenderingIntent::Perceptual
    };
    let options = TransformOptions {
        rendering_intent: intent,
        ..TransformOptions::default()
    };
    let transform = match source.create_in_place_transform_f32(Layout::Rgb, &target, options) {
        Ok(t) => t,
        Err(e) => {
            log::debug!("Skipping f32 ICC transform: couldn't create transform ({e})");
            return;
        }
    };

    let start = Instant::now();
    if let Err(e) = transform.transform(rgb) {
        log::debug!("f32 ICC transform failed: {e}");
        return;
    }
    let pixel_count = rgb.len() / 3;
    let source_desc = profile_description(source);
    let target_desc = profile_description(&target);
    let intent_name = if use_relative_colorimetric {
        "relative"
    } else {
        "perceptual"
    };
    let pixel_count_fmt = format_with_separators(pixel_count);
    log::debug!(
        "ICC f32 transform: {source_desc} -> {target_desc}, {intent_name} ({pixel_count_fmt} pixels) in {}ms",
        start.elapsed().as_millis()
    );
}

/// Format a number with thousands separators (e.g., 24000000 -> "24,000,000").
fn format_with_separators(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

/// Check if two ICC profiles are byte-identical.
pub fn profiles_match(a: &[u8], b: &[u8]) -> bool {
    a == b
}

/// Largest per-channel 8-bit difference a transform may produce **over
/// [`probe_lattice`]** and still count as a no-op. One step out of 255 is below
/// what any display resolves, and it's the size of the rounding noise two
/// encodings of the same color space produce against each other.
///
/// A sampled lattice under-reports, so the real bound is a step or two rather
/// than exactly one: the true maximum for system sRGB → Display P3 is 118 where
/// the probe finds 105. That's the accepted limit of the approach, measured in
/// `docs/notes/icc-negligible-transform-probe.md`. Don't read the skip as a
/// promise that nothing moves.
const NEGLIGIBLE_DELTA: u8 = 1;

/// Steps per channel in [`probe_lattice`]. 18 values spanning 0..=255 land on
/// both endpoints, so every primary, every secondary, and the whole neutral
/// axis is in the probe.
const PROBE_STEPS: u16 = 18;

/// An RGBA8 lattice covering the cube, used to ask a built transform whether it
/// actually moves any pixels. Built once; callers copy it into a scratch buffer.
fn probe_lattice() -> &'static [u8] {
    static LATTICE: OnceLock<Vec<u8>> = OnceLock::new();
    LATTICE.get_or_init(|| {
        let step = 255 / (PROBE_STEPS - 1);
        let levels: Vec<u8> = (0..PROBE_STEPS)
            .map(|i| (i * step).min(255) as u8)
            .collect();
        let mut out = Vec::with_capacity(levels.len().pow(3) * 4);
        for &r in &levels {
            for &g in &levels {
                for &b in &levels {
                    out.extend_from_slice(&[r, g, b, 255]);
                }
            }
        }
        out
    })
}

/// True if the transform can't move any probe channel by more than
/// [`NEGLIGIBLE_DELTA`], meaning running it over the real image would burn time
/// to change nothing a viewer could see.
///
/// [`profiles_match`] alone isn't enough, because two encodings of the same
/// color space aren't byte-equal. macOS tags exports with a 3,144-byte sRGB
/// profile whose transfer curve is a 1,024-entry table; [`srgb_icc_bytes`]
/// generates a 612-byte one with the parametric curve. Both describe sRGB, and
/// converting between them moves a channel by at most 1 while costing ~42 ms on
/// a 24 MP image.
///
/// Probing the built transform beats comparing parsed colorants and curves: it
/// answers the question that actually matters, it needs no tolerance tuning per
/// field, and it works for LUT-based profiles where no field-by-field
/// comparison would. It costs ~11 µs, against a transform that already had to
/// be built. Genuinely different profiles aren't close to the threshold: system
/// sRGB to Display P3 moves a probe channel by 105.
fn transform_is_negligible(transform: &(dyn InPlaceTransformExecutor<u8> + Send + Sync)) -> bool {
    let reference = probe_lattice();
    let mut probe = reference.to_vec();
    if transform.transform(&mut probe).is_err() {
        // Let the real call report the failure with the pixel count in hand.
        return false;
    }
    reference
        .iter()
        .zip(probe.iter())
        .all(|(a, b)| a.abs_diff(*b) <= NEGLIGIBLE_DELTA)
}

/// Extract a human-readable description from an ICC profile, for logging.
fn profile_description(profile: &ColorProfile) -> String {
    use moxcms::ProfileText;
    let desc = match profile.description.as_ref() {
        Some(ProfileText::PlainString(s)) => Some(s.as_str()),
        Some(ProfileText::Description(d)) => {
            if !d.unicode_string.is_empty() {
                Some(d.unicode_string.as_str())
            } else {
                Some(d.ascii_string.as_str())
            }
        }
        Some(ProfileText::Localizable(v)) => v.first().map(|ls| ls.value.as_str()),
        None => None,
    };
    desc.unwrap_or("unknown").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apple's Adobe RGB (1998) ICC profile (560 bytes). Embedded so tests run without filesystem
    /// access. If you swap the color library, these tests verify the replacement produces the
    /// same output.
    #[rustfmt::skip]
    const ADOBE_RGB_ICC: &[u8] = &[
        0x00, 0x00, 0x02, 0x30, 0x41, 0x44, 0x42, 0x45, 0x02, 0x10, 0x00, 0x00, 0x6d, 0x6e, 0x74, 0x72,
        0x52, 0x47, 0x42, 0x20, 0x58, 0x59, 0x5a, 0x20, 0x07, 0xd0, 0x00, 0x08, 0x00, 0x0b, 0x00, 0x13,
        0x00, 0x33, 0x00, 0x3b, 0x61, 0x63, 0x73, 0x70, 0x41, 0x50, 0x50, 0x4c, 0x00, 0x00, 0x00, 0x00,
        0x6e, 0x6f, 0x6e, 0x65, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf6, 0xd6, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xd3, 0x2d,
        0x41, 0x44, 0x42, 0x45, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x0a, 0x63, 0x70, 0x72, 0x74, 0x00, 0x00, 0x00, 0xfc, 0x00, 0x00, 0x00, 0x32,
        0x64, 0x65, 0x73, 0x63, 0x00, 0x00, 0x01, 0x30, 0x00, 0x00, 0x00, 0x6b, 0x77, 0x74, 0x70, 0x74,
        0x00, 0x00, 0x01, 0x9c, 0x00, 0x00, 0x00, 0x14, 0x62, 0x6b, 0x70, 0x74, 0x00, 0x00, 0x01, 0xb0,
        0x00, 0x00, 0x00, 0x14, 0x72, 0x54, 0x52, 0x43, 0x00, 0x00, 0x01, 0xc4, 0x00, 0x00, 0x00, 0x0e,
        0x67, 0x54, 0x52, 0x43, 0x00, 0x00, 0x01, 0xd4, 0x00, 0x00, 0x00, 0x0e, 0x62, 0x54, 0x52, 0x43,
        0x00, 0x00, 0x01, 0xe4, 0x00, 0x00, 0x00, 0x0e, 0x72, 0x58, 0x59, 0x5a, 0x00, 0x00, 0x01, 0xf4,
        0x00, 0x00, 0x00, 0x14, 0x67, 0x58, 0x59, 0x5a, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0x00, 0x14,
        0x62, 0x58, 0x59, 0x5a, 0x00, 0x00, 0x02, 0x1c, 0x00, 0x00, 0x00, 0x14, 0x74, 0x65, 0x78, 0x74,
        0x00, 0x00, 0x00, 0x00, 0x43, 0x6f, 0x70, 0x79, 0x72, 0x69, 0x67, 0x68, 0x74, 0x20, 0x32, 0x30,
        0x30, 0x30, 0x20, 0x41, 0x64, 0x6f, 0x62, 0x65, 0x20, 0x53, 0x79, 0x73, 0x74, 0x65, 0x6d, 0x73,
        0x20, 0x49, 0x6e, 0x63, 0x6f, 0x72, 0x70, 0x6f, 0x72, 0x61, 0x74, 0x65, 0x64, 0x00, 0x00, 0x00,
        0x64, 0x65, 0x73, 0x63, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x41, 0x64, 0x6f, 0x62,
        0x65, 0x20, 0x52, 0x47, 0x42, 0x20, 0x28, 0x31, 0x39, 0x39, 0x38, 0x29, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x58, 0x59, 0x5a, 0x20,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf3, 0x51, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x16, 0xcc,
        0x58, 0x59, 0x5a, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x63, 0x75, 0x72, 0x76, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x02, 0x33, 0x00, 0x00, 0x63, 0x75, 0x72, 0x76, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x02, 0x33, 0x00, 0x00, 0x63, 0x75, 0x72, 0x76, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x02, 0x33, 0x00, 0x00, 0x58, 0x59, 0x5a, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x9c, 0x18,
        0x00, 0x00, 0x4f, 0xa5, 0x00, 0x00, 0x04, 0xfc, 0x58, 0x59, 0x5a, 0x20, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x34, 0x8d, 0x00, 0x00, 0xa0, 0x2c, 0x00, 0x00, 0x0f, 0x95, 0x58, 0x59, 0x5a, 0x20,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x26, 0x31, 0x00, 0x00, 0x10, 0x2f, 0x00, 0x00, 0xbe, 0x9c,
    ];

    /// Known Adobe RGB -> sRGB transform results (verified against ImageMagick).
    /// Each entry: (input RGBA, expected output RGBA). Alpha is always preserved.
    ///
    /// These values come from the real-world test where we created an Adobe RGB JPEG with
    /// ImageMagick, displayed it in prvw, and verified pixel-exact matches. If you swap the
    /// color library (lcms2 -> qcms/moxcms), allow +/-1 tolerance per channel for rounding
    /// differences between implementations.
    const TOLERANCE: u8 = 1;

    fn srgb_icc() -> &'static [u8] {
        srgb_icc_bytes()
    }

    fn assert_pixel_near(actual: [u8; 4], expected: [u8; 4], label: &str) {
        for (ch, (a, e)) in ["R", "G", "B", "A"]
            .iter()
            .zip(actual.iter().zip(expected.iter()))
        {
            let diff = (*a as i16 - *e as i16).unsigned_abs() as u8;
            assert!(
                diff <= TOLERANCE,
                "{label}: {ch} channel mismatch: got {a}, expected {e} (diff {diff}, tolerance {TOLERANCE})"
            );
        }
    }

    #[test]
    fn adobe_rgb_to_srgb_known_values() {
        // Adobe RGB (146, 0, 0) -> sRGB (172, 0, 0): red is the most affected channel
        let mut red = [146, 0, 0, 255];
        transform_icc(&mut red, ADOBE_RGB_ICC, srgb_icc(), false);
        assert_pixel_near(red, [172, 0, 0, 255], "red");

        // Adobe RGB (0, 147, 0) -> sRGB (0, 148, 0): green barely changes
        let mut green = [0, 147, 0, 255];
        transform_icc(&mut green, ADOBE_RGB_ICC, srgb_icc(), false);
        assert_pixel_near(green, [0, 148, 0, 255], "green");

        // Adobe RGB (0, 0, 146) -> sRGB (0, 0, 150): blue shifts slightly
        let mut blue = [0, 0, 146, 255];
        transform_icc(&mut blue, ADOBE_RGB_ICC, srgb_icc(), false);
        assert_pixel_near(blue, [0, 0, 150, 255], "blue");
    }

    #[test]
    fn alpha_channel_preserved() {
        let mut pixel = [146, 0, 0, 128];
        transform_icc(&mut pixel, ADOBE_RGB_ICC, srgb_icc(), false);
        assert_eq!(pixel[3], 128, "alpha must be preserved");
    }

    #[test]
    fn matching_profiles_skip_transform() {
        let mut pixel = [200, 100, 50, 255];
        let original = pixel;
        transform_icc(&mut pixel, ADOBE_RGB_ICC, ADOBE_RGB_ICC, false);
        assert_eq!(pixel, original, "identical profiles should be a no-op");
    }

    #[test]
    fn profiles_match_identical() {
        assert!(profiles_match(ADOBE_RGB_ICC, ADOBE_RGB_ICC));
    }

    #[test]
    fn profiles_match_different() {
        assert!(!profiles_match(ADOBE_RGB_ICC, srgb_icc()));
    }

    /// The startup path (`color::State::from_settings`) calls this on every
    /// platform, so it has to produce bytes without touching the filesystem.
    #[test]
    fn srgb_bytes_are_a_parseable_icc_profile() {
        let bytes = srgb_icc();
        assert!(bytes.len() > 128, "ICC blob suspiciously small");
        assert_eq!(
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize,
            bytes.len(),
            "ICC header size doesn't match blob length"
        );
        let parsed = ColorProfile::new_from_slice(bytes).expect("should parse back");
        assert_eq!(parsed.color_space, moxcms::DataColorSpace::Rgb);
    }

    /// An sRGB profile shaped the way operating systems ship it: same
    /// colorimetry, but the transfer curve as a 1,024-entry table instead of
    /// the parametric form. This is what macOS tags exports with, so it's the
    /// profile most likely to arrive as a source alongside our generated one
    /// as the target.
    fn table_trc_srgb_icc() -> Vec<u8> {
        let mut profile = ColorProfile::new_srgb();
        let lut: Vec<u16> = (0..1024)
            .map(|i| {
                let linear = i as f32 / 1023.0;
                let encoded = if linear <= 0.003_130_8 {
                    linear * 12.92
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                };
                (encoded * 65535.0).round() as u16
            })
            .collect();
        let curve = moxcms::ToneReprCurve::Lut(lut);
        profile.red_trc = Some(curve.clone());
        profile.green_trc = Some(curve.clone());
        profile.blue_trc = Some(curve);
        profile.encode().expect("table-TRC sRGB encodes cleanly")
    }

    /// Two encodings of sRGB aren't byte-equal, so `profiles_match` lets them
    /// through. The transform they build has to be recognised as a no-op, or
    /// every system-tagged image pays a full pass for nothing.
    #[test]
    fn equivalent_srgb_encodings_skip_the_transform() {
        let table_srgb = table_trc_srgb_icc();
        assert!(
            !profiles_match(&table_srgb, srgb_icc()),
            "the two encodings differ byte-wise; that's the case under test"
        );

        let mut pixels = [200, 100, 50, 255, 12, 200, 240, 128];
        let original = pixels;
        transform_icc(&mut pixels, &table_srgb, srgb_icc(), false);
        assert_eq!(
            pixels, original,
            "an sRGB-to-sRGB transform must leave the buffer untouched"
        );
    }

    /// The negligible check must not swallow a real conversion.
    #[test]
    fn different_color_spaces_still_transform() {
        let mut pixel = [146, 0, 0, 255];
        transform_icc(&mut pixel, ADOBE_RGB_ICC, srgb_icc(), false);
        assert_ne!(
            pixel,
            [146, 0, 0, 255],
            "Adobe RGB to sRGB is a real conversion and must run"
        );
    }

    #[test]
    fn probe_lattice_covers_the_cube_corners() {
        let lattice = probe_lattice();
        assert_eq!(lattice.len(), (PROBE_STEPS as usize).pow(3) * 4);
        let has = |rgb: [u8; 3]| {
            lattice
                .chunks_exact(4)
                .any(|p| p[0] == rgb[0] && p[1] == rgb[1] && p[2] == rgb[2])
        };
        for corner in [
            [0, 0, 0],
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 0],
            [255, 0, 255],
            [0, 255, 255],
            [255, 255, 255],
        ] {
            assert!(has(corner), "lattice is missing corner {corner:?}");
        }
    }

    #[test]
    fn malformed_source_is_noop() {
        let mut pixel = [200, 100, 50, 255];
        let original = pixel;
        transform_icc(&mut pixel, b"not a real ICC profile", srgb_icc(), false);
        assert_eq!(
            pixel, original,
            "malformed source profile should be a no-op"
        );
    }

    #[test]
    fn empty_source_is_noop() {
        let mut pixel = [200, 100, 50, 255];
        let original = pixel;
        transform_icc(&mut pixel, &[], srgb_icc(), false);
        assert_eq!(pixel, original, "empty source profile should be a no-op");
    }

    #[test]
    fn multi_pixel_transform() {
        // 3 pixels: red, green, blue in Adobe RGB
        let mut pixels = [
            146, 0, 0, 255, // red
            0, 147, 0, 255, // green
            0, 0, 146, 255, // blue
        ];
        transform_icc(&mut pixels, ADOBE_RGB_ICC, srgb_icc(), false);

        assert_pixel_near(
            [pixels[0], pixels[1], pixels[2], pixels[3]],
            [172, 0, 0, 255],
            "pixel 0 red",
        );
        assert_pixel_near(
            [pixels[4], pixels[5], pixels[6], pixels[7]],
            [0, 148, 0, 255],
            "pixel 1 green",
        );
        assert_pixel_near(
            [pixels[8], pixels[9], pixels[10], pixels[11]],
            [0, 0, 150, 255],
            "pixel 2 blue",
        );
    }
}
