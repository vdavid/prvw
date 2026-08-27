//! The display-matching transform, end to end: decode a file, read its embedded ICC profile,
//! transform it into the profile of the display it's about to be shown on.
//!
//! **These run on every platform, and that is the point.** They used to be macOS-only, because
//! they read their target profiles out of `/System/Library/ColorSync/Profiles/`. `moxcms`
//! generates them now, the same argument `color::srgb_icc_bytes` makes for the app's own target:
//! nothing to license, nothing to bundle, nothing to keep in sync, and nothing that only exists on
//! one operating system.
//!
//! What that buys is the thing `docs/specs/cross-platform-plan.md` asks for under M2. Colour is
//! the one milestone this project's test setup can't verify by looking: there's no HDR display and
//! no calibrated Windows monitor here. So the reference pixels below were computed once, on a Mac,
//! and every platform has to reproduce them exactly. `moxcms` is pure Rust with no OS colour
//! plumbing under it, so a difference means a real difference. Running these on a Windows box for
//! the first time is then a confirmation pass rather than a discovery pass.
//!
//! What they deliberately don't cover: which profile each OS *hands* us. That's
//! `color::display_profile`'s job and it needs the real machine.

use std::path::Path;

use moxcms::{ColorProfile, Layout, RenderingIntent, TransformOptions};

/// Load an image file and decode it to RGBA8 bytes using zune-jpeg (same as prvw).
/// Returns (rgba_data, width, height, icc_profile_bytes).
fn decode_jpeg_with_icc(path: &Path) -> (Vec<u8>, u32, u32, Option<Vec<u8>>) {
    let bytes = std::fs::read(path).unwrap();
    let options = zune_core::options::DecoderOptions::new_fast();
    let cursor = std::io::Cursor::new(bytes);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(cursor, options);
    let rgb = decoder.decode().unwrap();
    let icc = decoder.icc_profile();
    let info = decoder.info().unwrap();
    let width = info.width as u32;
    let height = info.height as u32;

    let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
    for chunk in rgb.chunks_exact(3) {
        rgba.push(chunk[0]);
        rgba.push(chunk[1]);
        rgba.push(chunk[2]);
        rgba.push(255);
    }

    (rgba, width, height, icc)
}

/// The sRGB profile the app itself uses as a target when display matching is off. Generated, so
/// it's byte-identical on every platform.
fn srgb_profile() -> Vec<u8> {
    ColorProfile::new_srgb().encode().unwrap()
}

/// A Display P3 profile, standing in for the wide-gamut monitor a photographer actually owns.
fn display_p3_profile() -> Vec<u8> {
    ColorProfile::new_display_p3().encode().unwrap()
}

/// Minimal moxcms transform wrapper for tests (mirrors what prvw's `color::transform_icc` does).
fn moxcms_transform(
    rgba: &mut [u8],
    source_icc: &[u8],
    target_icc: &[u8],
    intent: RenderingIntent,
) {
    let source = ColorProfile::new_from_slice(source_icc).unwrap();
    let target = ColorProfile::new_from_slice(target_icc).unwrap();
    let options = TransformOptions {
        rendering_intent: intent,
        ..TransformOptions::default()
    };
    let transform = source
        .create_in_place_transform_8bit(Layout::Rgba, &target, options)
        .unwrap();
    transform.transform(rgba).unwrap();
}

/// The reference table. Computed on macOS on 2026-08-27 and asserted byte-exactly everywhere
/// after: `(Display P3 input, sRGB output)`.
///
/// **Read a failure here as a real colour change, not as flakiness.** There is no floating-point
/// tolerance on purpose. Every step between these two columns is `moxcms`, which is pure Rust with
/// no platform colour plumbing under it, so the same bytes in have to give the same bytes out on a
/// Mac, on Windows, and on Linux. If this ever differs per platform, something in the pipeline is
/// reading the operating system when it shouldn't be.
const P3_TO_SRGB: [([u8; 3], [u8; 3]); 5] = [
    // A warm mid-tone: the largest move in the table, and the one a portrait would show.
    ([200, 100, 50], [214, 92, 30]),
    // Neutral grey has the same coordinates in both, since both are D65.
    ([128, 128, 128], [128, 128, 128]),
    ([0, 200, 120], [0, 203, 111]),
    // Fully saturated P3 red is outside sRGB, so it lands on the gamut edge.
    ([255, 0, 0], [255, 0, 0]),
    ([20, 40, 220], [12, 40, 229]),
];

#[test]
fn display_p3_pixels_land_on_known_srgb_bytes() {
    let p3 = display_p3_profile();
    let srgb = srgb_profile();

    for (input, expected) in P3_TO_SRGB {
        let mut pixel = [input[0], input[1], input[2], 255];
        moxcms_transform(&mut pixel, &p3, &srgb, RenderingIntent::Perceptual);
        assert_eq!(
            [pixel[0], pixel[1], pixel[2]],
            expected,
            "Display P3 {input:?} should transform to sRGB {expected:?}"
        );
        assert_eq!(pixel[3], 255, "alpha must pass through untouched");
    }
}

/// The "Relative colorimetric" toggle in Settings → Color. Both profiles here are matrix-and-curve
/// profiles with no perceptual table, so there's nothing for the intent to select between and the
/// two answers agree. Pinned because a `moxcms` release that started diverging here would change
/// what that toggle does, silently.
#[test]
fn a_matrix_profile_answers_both_rendering_intents_the_same() {
    let p3 = display_p3_profile();
    let srgb = srgb_profile();

    for (input, expected) in P3_TO_SRGB {
        let mut pixel = [input[0], input[1], input[2], 255];
        moxcms_transform(
            &mut pixel,
            &p3,
            &srgb,
            RenderingIntent::RelativeColorimetric,
        );
        assert_eq!(
            [pixel[0], pixel[1], pixel[2]],
            expected,
            "relative colorimetric should match perceptual for {input:?} between two matrix profiles"
        );
    }
}

#[test]
fn a_p3_image_transforms_differently_for_an_srgb_and_a_p3_display() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/p3_red_64x64.jpg");
    assert!(
        fixture.exists(),
        "Test fixture missing: {}",
        fixture.display()
    );

    let (rgba_original, _w, _h, icc) = decode_jpeg_with_icc(&fixture);
    let source_icc = icc.expect("Test image should have an embedded ICC profile");

    let mut rgba_srgb = rgba_original.clone();
    moxcms_transform(
        &mut rgba_srgb,
        &source_icc,
        &srgb_profile(),
        RenderingIntent::Perceptual,
    );

    // Onto a P3 display this is near enough a no-op, since the image is already P3.
    let mut rgba_p3 = rgba_original.clone();
    moxcms_transform(
        &mut rgba_p3,
        &source_icc,
        &display_p3_profile(),
        RenderingIntent::Perceptual,
    );

    let srgb_pixel = [rgba_srgb[0], rgba_srgb[1], rgba_srgb[2]];
    let p3_pixel = [rgba_p3[0], rgba_p3[1], rgba_p3[2]];
    let original_pixel = [rgba_original[0], rgba_original[1], rgba_original[2]];

    assert_ne!(
        srgb_pixel, p3_pixel,
        "P3->sRGB and P3->P3 should produce different RGB values for a saturated P3 red"
    );

    for (ch, (&got, &expected)) in ["R", "G", "B"]
        .iter()
        .zip(p3_pixel.iter().zip(original_pixel.iter()))
    {
        let diff = (got as i16 - expected as i16).unsigned_abs();
        assert!(
            diff <= 1,
            "P3->P3 transform should be near-identity: {ch} got {got}, expected ~{expected} (diff {diff})"
        );
    }
}

#[test]
fn an_srgb_image_on_an_srgb_display_comes_out_unchanged() {
    let srgb = srgb_profile();
    let mut pixel = [200u8, 100, 50, 255];
    let original = pixel;

    // `color::profiles_match` short-circuits this in the app; the transform itself has to be a
    // near-identity too, or the skip would be hiding a difference rather than saving time.
    moxcms_transform(&mut pixel, &srgb, &srgb, RenderingIntent::Perceptual);

    for (i, (a, b)) in pixel.iter().zip(original.iter()).enumerate() {
        let diff = (*a as i16 - *b as i16).unsigned_abs();
        assert!(
            diff <= 1,
            "sRGB->sRGB should be near-identity: channel {i} got {a}, expected {b}"
        );
    }
}
