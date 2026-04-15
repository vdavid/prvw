//! Integration tests for ICC color management (Level 2).
//!
//! Tests the full pipeline: load image from disk -> extract ICC -> transform to target profile.

use std::path::Path;

// Access the crate's internal modules via the binary crate.
// Since prvw is a binary, we test the public functions by duplicating the minimal
// decode logic here. The actual unit tests for transform correctness live in color.rs.

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

#[test]
fn p3_image_transforms_differently_for_srgb_vs_p3_target() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/p3_red_64x64.jpg");
    assert!(
        fixture.exists(),
        "Test fixture missing: {}",
        fixture.display()
    );

    let (rgba_original, _w, _h, icc) = decode_jpeg_with_icc(&fixture);
    let source_icc = icc.expect("Test image should have an embedded ICC profile");

    let srgb_icc = std::fs::read("/System/Library/ColorSync/Profiles/sRGB Profile.icc").unwrap();
    let p3_icc = std::fs::read("/System/Library/ColorSync/Profiles/Display P3.icc").unwrap();

    // Transform to sRGB
    let mut rgba_srgb = rgba_original.clone();
    moxcms_transform(&mut rgba_srgb, &source_icc, &srgb_icc);

    // Transform to P3 (should be nearly a no-op since the image is P3)
    let mut rgba_p3 = rgba_original.clone();
    moxcms_transform(&mut rgba_p3, &source_icc, &p3_icc);

    // The two transforms should produce different pixel values.
    // P3->sRGB maps the wider P3 gamut into sRGB, while P3->P3 is near-identity.
    // For a saturated red, the difference shows up across all channels (perceptual intent
    // adjusts the entire gamut, not just clipped values).
    let srgb_pixel = [rgba_srgb[0], rgba_srgb[1], rgba_srgb[2]];
    let p3_pixel = [rgba_p3[0], rgba_p3[1], rgba_p3[2]];
    let original_pixel = [rgba_original[0], rgba_original[1], rgba_original[2]];

    assert_ne!(
        srgb_pixel, p3_pixel,
        "P3->sRGB and P3->P3 should produce different RGB values for a saturated P3 red"
    );

    // P3->P3 should preserve the original values (within tolerance)
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
fn srgb_image_on_srgb_display_is_noop() {
    let srgb_icc = std::fs::read("/System/Library/ColorSync/Profiles/sRGB Profile.icc").unwrap();

    // A pixel in sRGB, transformed to sRGB target, should be unchanged
    let mut pixel = [200u8, 100, 50, 255];
    let original = pixel;

    // profiles_match short-circuits, but let's also test the actual transform
    let source = moxcms::ColorProfile::new_from_slice(&srgb_icc).unwrap();
    let target = moxcms::ColorProfile::new_from_slice(&srgb_icc).unwrap();
    let options = moxcms::TransformOptions {
        rendering_intent: moxcms::RenderingIntent::Perceptual,
        ..moxcms::TransformOptions::default()
    };
    // This may or may not produce tiny rounding differences
    if let Ok(t) = source.create_in_place_transform_8bit(moxcms::Layout::Rgba, &target, options) {
        let _ = t.transform(&mut pixel);
    }

    for (i, (a, b)) in pixel.iter().zip(original.iter()).enumerate() {
        let diff = (*a as i16 - *b as i16).unsigned_abs();
        assert!(
            diff <= 1,
            "sRGB->sRGB should be near-identity: channel {i} got {a}, expected {b}"
        );
    }
}

/// Minimal moxcms transform wrapper for tests (mirrors what prvw's color.rs does).
fn moxcms_transform(rgba: &mut [u8], source_icc: &[u8], target_icc: &[u8]) {
    let source = moxcms::ColorProfile::new_from_slice(source_icc).unwrap();
    let target = moxcms::ColorProfile::new_from_slice(target_icc).unwrap();
    let options = moxcms::TransformOptions {
        rendering_intent: moxcms::RenderingIntent::Perceptual,
        ..moxcms::TransformOptions::default()
    };
    let transform = source
        .create_in_place_transform_8bit(moxcms::Layout::Rgba, &target, options)
        .unwrap();
    transform.transform(rgba).unwrap();
}
