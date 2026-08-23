//! Generated image fixtures. Nothing is checked in: every test builds what it needs in a temp
//! dir, so the suite is hermetic and no path outside the repo's control is involved.

/// The default fixture's edge length. The zoom, auto-fit, and window-geometry tests are
/// written against this natural fit size, so changing it moves their expectations.
pub const FIXTURE_SIZE: u32 = 1024;

/// Write the default fixture image: a vertical grayscale ramp.
///
/// The ramp gives a non-degenerate histogram, and one color per row lets PNG's row filter flatten
/// all but the first, so the file lands at ~23 KB and the app decodes it in ~30 ms against the old
/// 924 KB icon's ~75 ms. Writing it costs ~200 ms per test process, which is noise next to the
/// window and GPU setup every test already pays.
pub fn create_fixture_image(path: &std::path::Path) {
    let img = image::RgbaImage::from_fn(FIXTURE_SIZE, FIXTURE_SIZE, |_, y| {
        let value = (y * 256 / FIXTURE_SIZE) as u8;
        image::Rgba([value, value, value, 255])
    });
    img.save(path).expect("Failed to save the default fixture");
}

/// Create a solid white PNG image at the given path.
pub fn create_white_image(path: &std::path::Path, width: u32, height: u32) {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
    img.save(path).expect("Failed to save white test image");
}

/// Write a distinct solid-color PNG at `path` (shade derived from `seed`).
pub fn write_png(path: &std::path::Path, seed: u8) {
    let shade = seed.wrapping_mul(31);
    let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([shade, shade, shade, 255]));
    img.save(path).unwrap();
}

/// Build a temporary directory with `n` distinct PNG files. Returns the directory and the path
/// of the first image so the caller can launch the app pointing at it.
pub fn create_multi_image_dir(n: u32) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let mut first = None;
    for i in 0..n {
        let path = dir.path().join(format!("img-{i:02}.png"));
        // Vary the pixel color so each PNG is a distinct decode (avoids any cache deduping that
        // may key off content).
        let shade = (i as u8).wrapping_mul(17);
        let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([shade, shade, shade, 255]));
        img.save(&path).unwrap();
        if first.is_none() {
            first = Some(path);
        }
    }
    (dir, first.unwrap())
}

/// Build a tiny JPEG with a known EXIF segment in a temp dir. Returns the path. Used by the
/// EXIF-panel tests so we don't need a checked-in binary fixture.
pub fn create_jpeg_with_exif(dir: &std::path::Path) -> std::path::PathBuf {
    use little_exif::exif_tag::ExifTag as LeTag;
    use little_exif::metadata::Metadata;
    use little_exif::rational::uR64;

    let path = dir.join("with-exif.jpg");
    let img = image::RgbImage::from_pixel(8, 8, image::Rgb([180, 90, 60]));
    img.save(&path).expect("save test JPEG");

    let mut md = Metadata::new();
    md.set_tag(LeTag::Make("PrvwTest".into()));
    md.set_tag(LeTag::Model("Camera 9000".into()));
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
    md.write_to_file(&path).expect("inject EXIF");
    path
}
