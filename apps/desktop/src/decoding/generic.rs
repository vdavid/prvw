//! Generic decoding via the `image` crate (PNG, GIF, WebP, BMP, TIFF).
//!
//! ## ICC extraction ordering (subtle)
//!
//! `ImageReader::into_decoder()` returns `impl ImageDecoder`. `icc_profile()` takes
//! `&mut self`, and `DynamicImage::from_decoder()` consumes the decoder. So you
//! must call `icc_profile()` first, then `from_decoder()`. Reversing won't compile.

use std::io::Cursor;
use std::path::Path;
use std::time::Instant;

use image::ImageDecoder;

use crate::color;

use super::scene_linear::{self, PngColor};
use super::{DecodedImage, OutputDepth};

/// Decode a non-JPEG image via the `image` crate and color-manage to the target ICC profile.
/// 16-bit sources come back as `DisplaySdr` half-floats unless `depth` asks for 8 bits;
/// everything else, and every source at [`OutputDepth::Rgba8`], comes back RGBA8.
///
/// PQ and HLG PNGs and float TIFFs are light, not display values (`scene_linear`): with
/// `hdr_active` they come back `SceneLinear` for the shader to tone-map, otherwise tone-mapped
/// to SDR here.
pub(super) fn decode(
    path: &Path,
    bytes: Vec<u8>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    depth: OutputDepth,
    hdr_active: bool,
) -> Result<DecodedImage, String> {
    // An 8-bit caller can't carry HDR, whatever it was told.
    let hdr_active = hdr_active && depth == OutputDepth::Native;
    // Header chunks only, before the pixels: `image` doesn't pass `cICP` on.
    let png_color = scene_linear::probe_png(&bytes);
    let cursor = Cursor::new(&bytes);
    let reader = image::ImageReader::new(cursor)
        .with_guessed_format()
        .map_err(|e| {
            format!(
                "Couldn't identify format for {}: {e}",
                crate::paths::for_display(path)
            )
        })?;

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Couldn't decode {}: {e}", crate::paths::for_display(path)))?;

    let icc_profile = decoder.icc_profile().ok().flatten();

    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Couldn't decode {}: {e}", crate::paths::for_display(path)))?;

    // A `cICP` chunk outranks `iCCP` (PNG third edition).
    let cicp_icc;
    let source_icc = match png_color {
        PngColor::Hdr(signal) => {
            return Ok(scene_linear::decode_hdr_png(
                img,
                signal,
                target_icc,
                use_relative_colorimetric,
                hdr_active,
                depth,
            ));
        }
        PngColor::Sdr { icc, full_range } => {
            if !full_range {
                img = expanded_to_full_range(img);
            }
            cicp_icc = icc;
            cicp_icc.as_slice()
        }
        PngColor::Untagged => icc_profile
            .as_deref()
            .unwrap_or_else(|| color::srgb_icc_bytes()),
    };

    if is_float(&img) {
        return Ok(scene_linear::decode_float(
            img,
            icc_profile.as_deref(),
            target_icc,
            use_relative_colorimetric,
            hdr_active,
            depth,
        ));
    }

    if depth == OutputDepth::Native && is_16_bit(&img) {
        return Ok(decode_16_bit(
            img,
            source_icc,
            target_icc,
            use_relative_colorimetric,
        ));
    }

    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut rgba_data = rgba.into_raw();

    color::transform_icc(
        &mut rgba_data,
        source_icc,
        target_icc,
        use_relative_colorimetric,
    );

    Ok(DecodedImage::from_rgba8(width, height, rgba_data))
}

/// 16 bits per channel, integer. Float TIFFs aren't here: they may be HDR, which
/// is a different path (`scene_linear.rs`).
fn is_16_bit(img: &image::DynamicImage) -> bool {
    use image::DynamicImage as D;
    matches!(
        img,
        D::ImageLuma16(_) | D::ImageLumaA16(_) | D::ImageRgb16(_) | D::ImageRgba16(_)
    )
}

/// Linear-light float samples, which `image` produces for float TIFFs.
fn is_float(img: &image::DynamicImage) -> bool {
    use image::DynamicImage as D;
    matches!(img, D::ImageRgb32F(_) | D::ImageRgba32F(_))
}

/// A narrow-range SDR `cICP` PNG at full range, as RGBA16 so the scaling rounds once.
fn expanded_to_full_range(img: image::DynamicImage) -> image::DynamicImage {
    let bits = scene_linear::bits_per_channel(&img);
    let (width, height) = (img.width(), img.height());
    let mut samples = img.into_rgba16().into_raw();
    scene_linear::expand_narrow_range(&mut samples, bits);
    image::DynamicImage::ImageRgba16(
        image::ImageBuffer::from_raw(width, height, samples).expect("same dimensions"),
    )
}

/// The 16-bit SDR path: transform at 16 bits, then store the display-encoded
/// result as `DisplaySdr` half-floats (`color::display_sdr`). Rounding to 8
/// bits first, the way the 8-bit path does, stacks the transform's rounding on
/// top of the decode's, and smooth gradients band.
fn decode_16_bit(
    img: image::DynamicImage,
    source_icc: &[u8],
    target_icc: &[u8],
    use_relative_colorimetric: bool,
) -> DecodedImage {
    let start = Instant::now();
    let rgba = img.into_rgba16();
    let (width, height) = rgba.dimensions();
    let mut samples = rgba.into_raw();
    let widened = start.elapsed();

    color::transform_icc_16bit(
        &mut samples,
        source_icc,
        target_icc,
        use_relative_colorimetric,
    );
    let transformed = start.elapsed();

    color::display_sdr::rgba16_to_display_sdr_f16_inplace(&mut samples);
    log::debug!(
        "16-bit SDR stages [{:.1} ms total]: to_rgba16={:.1}, icc={:.1}, to_f16={:.1}",
        ms(start.elapsed()),
        ms(widened),
        ms(transformed - widened),
        ms(start.elapsed() - transformed),
    );
    DecodedImage::from_display_sdr_rgba16f(width, height, samples)
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoding::{OutputDepth, PixelBuffer, PixelEncoding};
    use half::f16;
    use moxcms::{ColorProfile, Layout, TransformOptions};

    /// Wide enough that a 16-bit ramp has ~16 samples per 8-bit level.
    const RAMP_WIDTH: u32 = 4096;

    /// A red ramp: R climbs 0..=65535 across the row, G and B stay at zero,
    /// alpha opaque. Off the neutral axis on purpose: sRGB → Display P3 moves
    /// it in every channel, where a grey ramp would map to itself.
    fn red_ramp_16() -> Vec<u16> {
        (0..RAMP_WIDTH)
            .flat_map(|x| {
                let r = (u64::from(x) * 65_535 / u64::from(RAMP_WIDTH - 1)) as u16;
                [r, 0, 0, 65_535]
            })
            .collect()
    }

    fn encode(image: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).expect("encode fixture");
        out.into_inner()
    }

    fn ramp_file(format: image::ImageFormat) -> Vec<u8> {
        let buffer =
            image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(RAMP_WIDTH, 1, red_ramp_16())
                .expect("ramp dimensions");
        encode(image::DynamicImage::ImageRgba16(buffer), format)
    }

    fn display_p3_icc() -> Vec<u8> {
        ColorProfile::new_display_p3().encode().expect("P3 encodes")
    }

    /// What the display should get with no rounding anywhere: the ramp through
    /// moxcms in f32, sRGB → Display P3, display-encoded `[0, 1]`.
    fn reference_p3(ramp: &[u16]) -> Vec<f32> {
        let mut values: Vec<f32> = ramp.iter().map(|&v| f32::from(v) / 65_535.0).collect();
        ColorProfile::new_srgb()
            .create_in_place_transform_f32(
                Layout::Rgba,
                &ColorProfile::new_display_p3(),
                TransformOptions::default(),
            )
            .expect("f32 transform")
            .transform(&mut values)
            .expect("f32 transform runs");
        values
    }

    /// Independent of `color::display_sdr`, so a bug there can't hide here.
    fn srgb_oetf(linear: f32) -> f32 {
        if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        }
    }

    /// The display-encoded `[0, 1]` values a decode stands for, whichever
    /// storage it chose.
    fn display_encoded(image: &DecodedImage) -> Vec<f32> {
        match (&image.pixels, &image.encoding) {
            (PixelBuffer::Rgba8(bytes), PixelEncoding::DisplaySdr) => {
                bytes.iter().map(|&b| f32::from(b) / 255.0).collect()
            }
            (PixelBuffer::Rgba16F(halfs), PixelEncoding::DisplaySdr) => halfs
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|px| {
                    let linear = |bits: u16| f16::from_bits(bits).to_f32();
                    [
                        srgb_oetf(linear(px[0])),
                        srgb_oetf(linear(px[1])),
                        srgb_oetf(linear(px[2])),
                        linear(px[3]),
                    ]
                })
                .collect(),
            (
                _,
                PixelEncoding::LinearHdrDisplay
                | PixelEncoding::GainMapped(_)
                | PixelEncoding::SceneLinear(_),
            ) => {
                panic!("a 16-bit PNG is never HDR")
            }
        }
    }

    /// Largest RGB error against the reference, in 8-bit steps.
    fn max_error_in_steps(got: &[f32], reference: &[f32]) -> f32 {
        got.as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .flat_map(|(g, r)| (0..3).map(move |c| (g[c] - r[c]).abs() * 255.0))
            .fold(0.0, f32::max)
    }

    /// Share of RGB samples whose final 8-bit value (what the surface writes)
    /// differs from rounding the reference once.
    fn off_by_a_step_share(got: &[f32], reference: &[f32]) -> f32 {
        let quantize = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let (mut off, mut total) = (0usize, 0usize);
        for (g, r) in got
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
        {
            for c in 0..3 {
                total += 1;
                if quantize(g[c]) != quantize(r[c]) {
                    off += 1;
                }
            }
        }
        off as f32 / total as f32
    }

    fn decode_bytes(bytes: Vec<u8>, target_icc: &[u8], depth: OutputDepth) -> DecodedImage {
        decode(Path::new("ramp"), bytes, target_icc, false, depth, false).expect("ramp decodes")
    }

    /// The banding this path exists to fix. A 16-bit ramp rounded to 8 bits
    /// before the ICC transform, then again after it, lands a step off the
    /// single-rounding answer on a good share of samples, and those
    /// off-by-ones are the bands. At 16 bits, the surface's one rounding is
    /// the only one left.
    #[test]
    fn a_16_bit_png_gradient_does_not_band_through_a_real_transform() {
        let image = decode_bytes(
            ramp_file(image::ImageFormat::Png),
            &display_p3_icc(),
            OutputDepth::Native,
        );
        let got = display_encoded(&image);
        let reference = reference_p3(&red_ramp_16());

        let max_error = max_error_in_steps(&got, &reference);
        let off_share = off_by_a_step_share(&got, &reference);
        assert!(
            max_error < 0.1,
            "max error {max_error:.3} steps, off-by-a-step share {off_share:.4}"
        );
        // Not zero: moxcms' 16-bit transform is a few hundredths of a step off
        // its f32 one, which tips samples sitting right at a rounding boundary.
        // Today's path measured 0.48 here (2026-10-13), the new one 0.017.
        assert!(off_share < 0.05, "off-by-a-step share {off_share:.4}");
        assert!(matches!(image.pixels, PixelBuffer::Rgba16F(_)));
        assert_eq!(image.encoding, PixelEncoding::DisplaySdr);
        assert!(
            !image.encoding.kind().is_hdr(),
            "16-bit SDR must not ask for the EDR surface"
        );
    }

    /// TIFF takes the same path: `image` hands back the same `DynamicImage`.
    #[test]
    fn a_16_bit_tiff_gradient_does_not_band_through_a_real_transform() {
        let image = decode_bytes(
            ramp_file(image::ImageFormat::Tiff),
            &display_p3_icc(),
            OutputDepth::Native,
        );
        let got = display_encoded(&image);
        let max_error = max_error_in_steps(&got, &reference_p3(&red_ramp_16()));
        assert!(max_error < 0.1, "max error {max_error:.3} steps");
        assert_eq!(image.encoding, PixelEncoding::DisplaySdr);
        assert!(matches!(image.pixels, PixelBuffer::Rgba16F(_)));
    }

    /// No embedded profile and an sRGB target: the transform is skipped, and
    /// the samples still come through at 16-bit precision.
    #[test]
    fn a_16_bit_png_keeps_its_precision_when_the_transform_is_skipped() {
        let image = decode_bytes(
            ramp_file(image::ImageFormat::Png),
            color::srgb_icc_bytes(),
            OutputDepth::Native,
        );
        let got = display_encoded(&image);
        let source: Vec<f32> = red_ramp_16()
            .iter()
            .map(|&v| f32::from(v) / 65_535.0)
            .collect();
        let max_error = max_error_in_steps(&got, &source);
        assert!(max_error < 0.1, "max error {max_error:.3} steps");
    }

    /// Today's path, spelled out: 8 bits first, then the transform.
    fn todays_path(file: &[u8], target_icc: &[u8]) -> Vec<u8> {
        let mut rgba = image::load_from_memory(file)
            .expect("fixture")
            .to_rgba8()
            .into_raw();
        color::transform_icc(&mut rgba, color::srgb_icc_bytes(), target_icc, false);
        rgba
    }

    /// Clipboard, print, and previews ask for bytes, and get exactly what they
    /// got before 16-bit decoding existed.
    #[test]
    fn a_16_bit_png_at_rgba8_depth_takes_todays_path() {
        let file = ramp_file(image::ImageFormat::Png);
        let target = display_p3_icc();
        let image = decode_bytes(file.clone(), &target, OutputDepth::Rgba8);
        let PixelBuffer::Rgba8(bytes) = &image.pixels else {
            panic!("an 8-bit caller must get RGBA8");
        };
        assert_eq!(bytes, &todays_path(&file, &target));
        assert_eq!(image.encoding, PixelEncoding::DisplaySdr);
    }

    /// Decode time of a 12 MP 16-bit file, today's path (`OutputDepth::Rgba8`
    /// is exactly it) against the 16-bit one. Numbers live in
    /// `docs/notes/hdr-images.md`. Run in release:
    /// `cargo test --release sixteen_bit_decode_perf -- --ignored --nocapture`, plus
    /// `RUST_LOG=prvw::decoding::generic=debug` for the per-stage split.
    #[test]
    #[ignore]
    fn sixteen_bit_decode_perf() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (width, height) = (4000u32, 3000u32);
        // A smooth two-axis gradient with a little noise, so the PNG compresses like a photo
        // rather than like a test pattern.
        let samples: Vec<u16> = (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    let noise =
                        ((x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 20) & 0x3ff;
                    [
                        (x * 65_535 / width) as u16,
                        (y * 65_535 / height) as u16,
                        (32_768 + noise) as u16,
                    ]
                })
            })
            .collect();
        let buffer = image::ImageBuffer::<image::Rgb<u16>, _>::from_raw(width, height, samples)
            .expect("dimensions");
        let image = image::DynamicImage::ImageRgb16(buffer);
        let targets = [
            ("Display P3 target", display_p3_icc()),
            (
                "sRGB target (transform skipped)",
                color::srgb_icc_bytes().to_vec(),
            ),
        ];
        for format in [image::ImageFormat::Png, image::ImageFormat::Tiff] {
            let file = encode(image.clone(), format);
            for (label, target) in &targets {
                let median = |depth: OutputDepth| {
                    let _ = decode_bytes(file.clone(), target, depth);
                    let mut runs: Vec<f64> = (0..7)
                        .map(|_| {
                            let bytes = file.clone();
                            let start = Instant::now();
                            let _ = decode_bytes(bytes, target, depth);
                            ms(start.elapsed())
                        })
                        .collect();
                    runs.sort_by(f64::total_cmp);
                    runs[runs.len() / 2]
                };
                let old = median(OutputDepth::Rgba8);
                let new = median(OutputDepth::Native);
                println!(
                    "{format:?} 12 MP RGB16, {label}: today {old:.1} ms, 16-bit {new:.1} ms ({:+.1}%)",
                    (new / old - 1.0) * 100.0
                );
            }
        }
    }

    /// 8-bit sources don't change at all, at either depth.
    #[test]
    fn an_8_bit_png_is_byte_identical_to_todays_path() {
        let rgba: Vec<u8> = (0..256u32 * 4).map(|i| (i * 37 % 256) as u8).collect();
        let source = image::RgbaImage::from_raw(256, 1, rgba).expect("dimensions");
        let file = encode(
            image::DynamicImage::ImageRgba8(source),
            image::ImageFormat::Png,
        );
        let target = display_p3_icc();
        for depth in [OutputDepth::Native, OutputDepth::Rgba8] {
            let image = decode_bytes(file.clone(), &target, depth);
            let PixelBuffer::Rgba8(bytes) = &image.pixels else {
                panic!("an 8-bit PNG must stay RGBA8 ({depth:?})");
            };
            assert_eq!(bytes, &todays_path(&file, &target), "{depth:?}");
            assert_eq!(image.encoding, PixelEncoding::DisplaySdr);
        }
    }
}
