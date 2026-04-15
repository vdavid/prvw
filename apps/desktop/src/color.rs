use moxcms::{ColorProfile, InPlaceTransformExecutor, Layout, RenderingIntent, TransformOptions};
use std::sync::OnceLock;
use std::time::Instant;

/// The macOS system sRGB ICC profile path. Always present on macOS.
const SRGB_PROFILE_PATH: &str = "/System/Library/ColorSync/Profiles/sRGB Profile.icc";

/// Returns the sRGB ICC profile bytes, loaded once from the macOS system profile.
/// Panics if the system profile is missing (should never happen on macOS).
pub fn srgb_icc_bytes() -> &'static [u8] {
    static SRGB: OnceLock<Vec<u8>> = OnceLock::new();
    SRGB.get_or_init(|| {
        std::fs::read(SRGB_PROFILE_PATH).unwrap_or_else(|e| {
            panic!("Couldn't read system sRGB profile at {SRGB_PROFILE_PATH}: {e}")
        })
    })
}

/// Transform RGBA8 pixels from a source ICC profile to a target ICC profile, in-place.
/// Skips the transform if the profiles match (byte-equal).
/// Silently returns on malformed profiles (the image displays as-is).
pub fn transform_icc(rgba: &mut [u8], source_icc: &[u8], target_icc: &[u8]) {
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

    let options = TransformOptions {
        rendering_intent: RenderingIntent::Perceptual,
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

    let start = Instant::now();
    if let Err(e) = transform.transform(rgba) {
        log::debug!("ICC transform failed: {e}");
        return;
    }
    let pixel_count = rgba.len() / 4;
    let source_desc = profile_description(&source);
    let target_desc = profile_description(&target);
    log::debug!(
        "ICC transform: {source_desc} -> {target_desc} ({pixel_count} pixels) in {}ms",
        start.elapsed().as_millis()
    );
}

/// Check if two ICC profiles are byte-identical.
pub fn profiles_match(a: &[u8], b: &[u8]) -> bool {
    a == b
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
        transform_icc(&mut red, ADOBE_RGB_ICC, srgb_icc());
        assert_pixel_near(red, [172, 0, 0, 255], "red");

        // Adobe RGB (0, 147, 0) -> sRGB (0, 148, 0): green barely changes
        let mut green = [0, 147, 0, 255];
        transform_icc(&mut green, ADOBE_RGB_ICC, srgb_icc());
        assert_pixel_near(green, [0, 148, 0, 255], "green");

        // Adobe RGB (0, 0, 146) -> sRGB (0, 0, 150): blue shifts slightly
        let mut blue = [0, 0, 146, 255];
        transform_icc(&mut blue, ADOBE_RGB_ICC, srgb_icc());
        assert_pixel_near(blue, [0, 0, 150, 255], "blue");
    }

    #[test]
    fn alpha_channel_preserved() {
        let mut pixel = [146, 0, 0, 128];
        transform_icc(&mut pixel, ADOBE_RGB_ICC, srgb_icc());
        assert_eq!(pixel[3], 128, "alpha must be preserved");
    }

    #[test]
    fn matching_profiles_skip_transform() {
        let mut pixel = [200, 100, 50, 255];
        let original = pixel;
        transform_icc(&mut pixel, ADOBE_RGB_ICC, ADOBE_RGB_ICC);
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

    #[test]
    fn malformed_source_is_noop() {
        let mut pixel = [200, 100, 50, 255];
        let original = pixel;
        transform_icc(&mut pixel, b"not a real ICC profile", srgb_icc());
        assert_eq!(
            pixel, original,
            "malformed source profile should be a no-op"
        );
    }

    #[test]
    fn empty_source_is_noop() {
        let mut pixel = [200, 100, 50, 255];
        let original = pixel;
        transform_icc(&mut pixel, &[], srgb_icc());
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
        transform_icc(&mut pixels, ADOBE_RGB_ICC, srgb_icc());

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
