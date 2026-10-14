//! PQ and HLG PNGs and float TIFFs: HDR stills whose samples are light rather than display values
//! (`docs/specs/hdr-images.md`, section 5).
//!
//! Each decodes to linear light relative to SDR white (1.0 = 203 nits) in its own primaries, then
//! takes one of three shapes:
//!
//! - **HDR active** (`raw_flags.hdr_output` and a display that can show HDR): `SceneLinear`
//!   half-floats, which shader mode 3 tone-maps per frame at the live headroom.
//! - **Otherwise** (an SDR display, HDR off, Linux, and every byte consumer): the same tone map on
//!   the CPU with SDR white as the target, then `moxcms` from the linear source into the display
//!   profile, RGBA8 `DisplaySdr`. A photo, where reading PQ code values as sRGB gave a grey wash.
//! - **A float TIFF that never passes SDR white** is SDR in a float container: `DisplaySdr`
//!   half-floats, M1's 16-bit shape, and never the EDR surface.
//!
//! A PNG's `cICP` chunk is read through the `png` crate, header chunks only, before `image`
//! decodes the pixels: `image` 0.25 doesn't pass it on. Per PNG's third edition it wins over
//! `iCCP`, `sRGB`, and `gAMA`, and an SDR `cICP` gets the profile it names
//! ([`PngColor::Sdr`]), which the normal SDR path then uses.

use std::io::Cursor;
use std::sync::OnceLock;
use std::time::Instant;

use half::f16;
use image::DynamicImage;
use moxcms::{
    CicpColorPrimaries, CicpProfile, ColorPrimaries, ColorProfile, MatrixCoefficients,
    ToneReprCurve, TransferCharacteristics,
};
use rayon::prelude::*;

use crate::color;
use crate::color::hdr::gain_map::{self, Mat3};
use crate::color::hdr::tone_map::{self, PeakHistogram, SceneLinearParams};
use crate::color::hdr::transfer::{self, REFERENCE_WHITE_NITS};
use crate::color::profiles::HdrDisplaySpace;

use super::{DecodedImage, OutputDepth};

/// What a PNG's `cICP` chunk asks for.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum PngColor {
    /// No `cICP`, or one a PNG can't carry: the ICC profile (or sRGB) decides, as it always has.
    Untagged,
    /// SDR code points. `icc` is the profile they name, and it replaces the file's own.
    Sdr { icc: Vec<u8>, full_range: bool },
    /// PQ or HLG.
    Hdr(HdrSignal),
}

/// A PQ or HLG PNG's code points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct HdrSignal {
    pub transfer: Transfer,
    pub primaries: u8,
    pub full_range: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transfer {
    Pq,
    Hlg,
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
/// ITU-T H.273's transfer characteristics for the two HDR signals.
const CICP_PQ: u8 = 16;
const CICP_HLG: u8 = 18;

/// Read a PNG's `cICP` (and, for the log, `cLLI` and `mDCV`) from the chunks before its first
/// `IDAT`. Anything that isn't a readable PNG is [`PngColor::Untagged`], and `image` reports the
/// real error.
pub(super) fn probe_png(bytes: &[u8]) -> PngColor {
    if !bytes.starts_with(&PNG_SIGNATURE) {
        return PngColor::Untagged;
    }
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_ignore_text_chunk(true);
    let Ok(reader) = decoder.read_info() else {
        return PngColor::Untagged;
    };
    let info = reader.info();
    if let Some(cll) = info.content_light_level {
        // Units of 0.0001 nits. Logged, never trusted: MaxCLL is often missing or wrong, so the
        // content peak is measured instead.
        log::debug!(
            "PNG cLLI: MaxCLL {:.0} nits, MaxFALL {:.0} nits",
            f64::from(cll.max_content_light_level) / 10_000.0,
            f64::from(cll.max_frame_average_light_level) / 10_000.0
        );
    }
    if let Some(mdcv) = info.mastering_display_color_volume {
        log::debug!(
            "PNG mDCV: mastered on a {:.0}-nit display",
            f64::from(mdcv.max_luminance) / 10_000.0
        );
    }
    classify(info.coding_independent_code_points)
}

fn classify(cicp: Option<png::CodingIndependentCodePoints>) -> PngColor {
    let Some(c) = cicp else {
        return PngColor::Untagged;
    };
    if c.matrix_coefficients != 0 {
        log::debug!(
            "PNG cICP names matrix coefficients {}, which a PNG can't carry (it's RGB); ignoring it",
            c.matrix_coefficients
        );
        return PngColor::Untagged;
    }
    let full_range = c.is_video_full_range_image;
    let transfer = match c.transfer_function {
        CICP_PQ => Transfer::Pq,
        CICP_HLG => Transfer::Hlg,
        other => {
            return match sdr_profile(c.color_primaries, other) {
                Some(icc) => PngColor::Sdr { icc, full_range },
                None => {
                    log::debug!(
                        "PNG cICP {}/{other} names no profile moxcms can build; ignoring it",
                        c.color_primaries
                    );
                    PngColor::Untagged
                }
            };
        }
    };
    PngColor::Hdr(HdrSignal {
        transfer,
        primaries: c.color_primaries,
        full_range,
    })
}

/// The ICC profile SDR code points name, or `None` when `moxcms` can't build it (reserved or
/// unspecified primaries, a transfer it has no curve for).
fn sdr_profile(primaries: u8, transfer: u8) -> Option<Vec<u8>> {
    let primaries = usable_primaries(primaries)?;
    let transfer = TransferCharacteristics::try_from(transfer).ok()?;
    ToneReprCurve::try_from(transfer).ok()?;
    ColorProfile::new_from_cicp(CicpProfile {
        color_primaries: primaries,
        transfer_characteristics: transfer,
        matrix_coefficients: MatrixCoefficients::Identity,
        full_range: true,
    })
    .encode()
    .ok()
}

/// `new_from_cicp` silently builds an empty profile for primaries without chromaticities, so they
/// have to be checked first.
fn usable_primaries(primaries: u8) -> Option<CicpColorPrimaries> {
    let primaries = CicpColorPrimaries::try_from(primaries).ok()?;
    ColorPrimaries::try_from(primaries).ok()?;
    Some(primaries)
}

/// Bits per color channel of a decoded image: what a narrow-range code is measured in.
pub(super) fn bits_per_channel(img: &DynamicImage) -> u32 {
    let color = img.color();
    u32::from(color.bytes_per_pixel()) * 8 / u32::from(color.channel_count())
}

/// Narrow ("video") range to full, in place, on RGBA16 samples from a source of `bits` per
/// channel: black at code 16 and white at 235, scaled to the bit depth. Alpha is never narrow.
/// Sub-8-bit sources (grey PNGs at 1, 2, or 4 bits) have no narrow range and are left alone.
pub(super) fn expand_narrow_range(samples: &mut [u16], bits: u32) {
    if !(8..=16).contains(&bits) {
        return;
    }
    let max_code = ((1u32 << bits) - 1) as f32;
    let step = (1u32 << (bits - 8)) as f32;
    let (black, range) = (16.0 * step, 219.0 * step);
    for px in samples.as_chunks_mut::<4>().0 {
        for v in &mut px[..3] {
            let code = f32::from(*v) / 65_535.0 * max_code;
            *v = (((code - black) / range).clamp(0.0, 1.0) * 65_535.0).round() as u16;
        }
    }
}

/// Pixels per rayon task: big enough to amortize a 1,024-bin histogram per task.
const CHUNK_PIXELS: usize = 16_384;

/// A PQ or HLG PNG that `image` decoded into `img`.
pub(super) fn decode_hdr_png(
    img: DynamicImage,
    signal: HdrSignal,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    hdr_active: bool,
    depth: OutputDepth,
) -> DecodedImage {
    let start = Instant::now();
    let bits = bits_per_channel(&img);
    let (width, height) = (img.width(), img.height());
    let mut samples = img.into_rgba16().into_raw();
    if !signal.full_range {
        expand_narrow_range(&mut samples, bits);
    }
    let widened = start.elapsed();

    let space = SourceSpace::from_cicp(signal.primaries);
    let weights = space.weights;
    let lut = signal_lut(signal.transfer);
    let to_light = |px: &[u16; 4]| -> [f32; 3] {
        let rgb = [0, 1, 2].map(|c| lut[usize::from(px[c])]);
        match signal.transfer {
            Transfer::Pq => rgb,
            Transfer::Hlg => transfer::hlg_ootf(rgb, weights),
        }
    };
    let alpha = |a: u16| f32::from(a) / 65_535.0;

    if hdr_active {
        // One pass, in place: the light, its luminance into the peak histogram, half-floats out.
        let peak = samples
            .par_chunks_mut(CHUNK_PIXELS * 4)
            .map(|chunk| {
                let mut histogram = PeakHistogram::default();
                for px in chunk.as_chunks_mut::<4>().0 {
                    let rgb = to_light(px);
                    histogram.add(luminance(rgb, weights));
                    *px = half_rgba(rgb, alpha(px[3]));
                }
                histogram
            })
            .reduce(PeakHistogram::default, PeakHistogram::merge)
            .peak();
        log_stages(signal.transfer, start, widened, peak, "HDR");
        return DecodedImage::from_scene_linear_rgba16f(width, height, samples, space.params(peak));
    }

    let mut rgb = vec![0.0f32; samples.len() / 4 * 3];
    let peak = samples
        .par_chunks(CHUNK_PIXELS * 4)
        .zip(rgb.par_chunks_mut(CHUNK_PIXELS * 3))
        .map(|(src, dst)| {
            let mut histogram = PeakHistogram::default();
            for (px, out) in src
                .as_chunks::<4>()
                .0
                .iter()
                .zip(dst.as_chunks_mut::<3>().0)
            {
                *out = to_light(px);
                histogram.add(luminance(*out, weights));
            }
            histogram
        })
        .reduce(PeakHistogram::default, PeakHistogram::merge)
        .peak();
    let image = sdr_fallback(
        width,
        height,
        rgb,
        |i| alpha(samples[i * 4 + 3]),
        peak,
        &space,
        target_icc,
        use_relative_colorimetric,
        depth,
    );
    log_stages(signal.transfer, start, widened, peak, "SDR fallback");
    image
}

fn log_stages(
    transfer: Transfer,
    start: Instant,
    widened: std::time::Duration,
    peak: f32,
    shape: &str,
) {
    log::debug!(
        "{transfer:?} PNG stages [{:.1} ms total, {shape}]: to_rgba16={:.1}, to_light={:.1}; content peak {peak:.2}× SDR white ({:.0} nits)",
        ms(start.elapsed()),
        ms(widened),
        ms(start.elapsed() - widened),
        peak * REFERENCE_WHITE_NITS,
    );
}

/// A float TIFF (`Rgb32F` / `Rgba32F`): linear light, 1.0 = SDR white. `icc` names the primaries
/// when it's a matrix-shaper profile; its curves are ignored, since the samples are linear.
pub(super) fn decode_float(
    img: DynamicImage,
    icc: Option<&[u8]>,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    hdr_active: bool,
    depth: OutputDepth,
) -> DecodedImage {
    let start = Instant::now();
    let (width, height) = (img.width(), img.height());
    let mut rgba = img.into_rgba32f().into_raw();
    let space = SourceSpace::from_icc(icc);
    let weights = space.weights;
    let histogram = rgba
        .par_chunks_mut(CHUNK_PIXELS * 4)
        .map(|chunk| {
            let mut histogram = PeakHistogram::default();
            for px in chunk.as_chunks_mut::<4>().0 {
                // A renderer's NaN or infinity would reach the texture as garbage.
                *px = px.map(|v| {
                    if v.is_nan() {
                        0.0
                    } else {
                        v.clamp(-F16_MAX, F16_MAX)
                    }
                });
                histogram.add(luminance([px[0], px[1], px[2]], weights));
            }
            histogram
        })
        .reduce(PeakHistogram::default, PeakHistogram::merge);
    // The percentile shapes the tone map; whether there's anything above SDR white at all is the
    // brightest pixel's call, or a few lamps would make a render "SDR" and clip.
    let (peak, max) = (histogram.peak(), histogram.max());
    let sdr = max <= SDR_PEAK;
    log::debug!(
        "Float TIFF: content peak {peak:.3}× SDR white, brightest {max:.3}×, {} in {:.1} ms",
        if sdr { "SDR" } else { "HDR" },
        ms(start.elapsed())
    );

    if !sdr && hdr_active {
        let halves = rgba
            .par_chunks(CHUNK_PIXELS * 4)
            .flat_map_iter(|chunk| {
                chunk
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|px| half_rgba([px[0], px[1], px[2]], px[3]))
                    .collect::<Vec<_>>()
            })
            .collect();
        return DecodedImage::from_scene_linear_rgba16f(width, height, halves, space.params(peak));
    }

    let rgb: Vec<f32> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();
    // An SDR float TIFF is the same rendition with nothing to tone-map (its peak fits SDR white).
    let alpha = |i: usize| rgba[i * 4 + 3];
    sdr_fallback(
        width,
        height,
        rgb,
        alpha,
        peak,
        &space,
        target_icc,
        use_relative_colorimetric,
        depth,
    )
}

/// The largest finite half-float.
const F16_MAX: f32 = 65_504.0;

/// A float TIFF peaking at or below this is SDR. A hair above 1.0, so white computed through
/// luminance weights that sum to 1.0000001 still counts.
const SDR_PEAK: f32 = 1.0 + 1.0 / 1024.0;

/// The CPU rendition for anything that can't show HDR: the tone map with SDR white as the target,
/// then the linear source into the display profile. The viewer (`OutputDepth::Native`) gets M1's
/// 16-bit `DisplaySdr` shape, because a strong tone curve followed by an 8-bit rounding is where
/// skies band; the byte consumers get RGBA8.
#[allow(clippy::too_many_arguments)] // One call shape for two decoders; the knobs are independent
fn sdr_fallback(
    width: u32,
    height: u32,
    mut rgb: Vec<f32>,
    alpha: impl Fn(usize) -> f32 + Sync,
    peak: f32,
    space: &SourceSpace,
    target_icc: &[u8],
    use_relative_colorimetric: bool,
    depth: OutputDepth,
) -> DecodedImage {
    let start = Instant::now();
    let weights = space.weights;
    let curve = tone_map::ToneCurve::new(peak, 1.0);
    rgb.par_chunks_mut(CHUNK_PIXELS * 3).for_each(|chunk| {
        for px in chunk.as_chunks_mut::<3>().0 {
            *px = curve.rgb(*px, weights);
        }
    });
    let toned = start.elapsed();
    color::transform_linear_f32_to_display(
        &mut rgb,
        &space.linear,
        target_icc,
        use_relative_colorimetric,
    );
    let transformed = start.elapsed();
    let image = match depth {
        OutputDepth::Rgba8 => {
            let rgba = quantize(&rgb, &alpha, |v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
            DecodedImage::from_rgba8(width, height, rgba)
        }
        OutputDepth::Native => {
            let mut samples = quantize(&rgb, &alpha, |v| {
                (v.clamp(0.0, 1.0) * 65_535.0).round() as u16
            });
            color::display_sdr::rgba16_to_display_sdr_f16_inplace(&mut samples);
            DecodedImage::from_display_sdr_rgba16f(width, height, samples)
        }
    };
    log::debug!(
        "SDR fallback stages [{:.1} ms total]: tone_map={:.1}, icc={:.1}, quantize={:.1} ({depth:?})",
        ms(start.elapsed()),
        ms(toned),
        ms(transformed - toned),
        ms(start.elapsed() - transformed),
    );
    image
}

/// Display-encoded RGB plus alpha into RGBA samples, in parallel.
fn quantize<T: Copy + Default + Send>(
    rgb: &[f32],
    alpha: &(impl Fn(usize) -> f32 + Sync),
    to: impl Fn(f32) -> T + Sync,
) -> Vec<T> {
    let mut rgba = vec![T::default(); rgb.len() / 3 * 4];
    rgba.par_chunks_mut(CHUNK_PIXELS * 4)
        .zip(rgb.par_chunks(CHUNK_PIXELS * 3))
        .enumerate()
        .for_each(|(chunk, (dst, src))| {
            let first = chunk * CHUNK_PIXELS;
            for (i, (out, px)) in dst
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<3>().0)
                .enumerate()
            {
                *out = [to(px[0]), to(px[1]), to(px[2]), to(alpha(first + i))];
            }
        });
    rgba
}

fn luminance(rgb: [f32; 3], weights: [f32; 3]) -> f32 {
    weights[0] * rgb[0] + weights[1] * rgb[1] + weights[2] * rgb[2]
}

fn half_rgba(rgb: [f32; 3], alpha: f32) -> [u16; 4] {
    [rgb[0], rgb[1], rgb[2], alpha].map(|v| f16::from_f32(v).to_bits())
}

/// Code value (as a 16-bit sample) to light: PQ straight to display light relative to SDR white,
/// HLG to normalized scene light, which the OOTF finishes per pixel. Built once per transfer.
fn signal_lut(transfer: Transfer) -> &'static [f32] {
    static PQ: OnceLock<Vec<f32>> = OnceLock::new();
    static HLG: OnceLock<Vec<f32>> = OnceLock::new();
    let build = |f: fn(f32) -> f32| -> Vec<f32> {
        (0..=u16::MAX)
            .map(|code| f(f32::from(code) / 65_535.0))
            .collect()
    };
    match transfer {
        Transfer::Pq => PQ.get_or_init(|| build(|e| transfer::pq_eotf(e) / REFERENCE_WHITE_NITS)),
        Transfer::Hlg => HLG.get_or_init(|| build(transfer::hlg_inverse_oetf)),
    }
}

/// The source's primaries, as mode 3 and the SDR fallback need them.
struct SourceSpace {
    /// The primaries with linear curves: what the SDR fallback converts from.
    linear: ColorProfile,
    to_display: Mat3,
    weights: [f32; 3],
}

impl SourceSpace {
    /// A PQ or HLG PNG's primaries. Unusable ones (reserved, unspecified) are taken as BT.2020,
    /// which is what BT.2100 signals use.
    fn from_cicp(primaries: u8) -> Self {
        let primaries = usable_primaries(primaries).unwrap_or_else(|| {
            log::debug!("PNG cICP primaries {primaries} are unusable; assuming BT.2020");
            CicpColorPrimaries::Bt2020
        });
        Self::linear_in(ColorProfile::new_from_cicp(CicpProfile {
            color_primaries: primaries,
            transfer_characteristics: TransferCharacteristics::Linear,
            matrix_coefficients: MatrixCoefficients::Identity,
            full_range: true,
        }))
    }

    /// A float TIFF's: the ICC's colorants when it's a matrix-shaper profile, BT.709 otherwise.
    fn from_icc(icc: Option<&[u8]>) -> Self {
        let profile = icc
            .and_then(|bytes| ColorProfile::new_from_slice(bytes).ok())
            .filter(ColorProfile::is_matrix_shaper)
            .unwrap_or_else(ColorProfile::new_srgb);
        Self::linear_in(profile)
    }

    fn linear_in(mut profile: ColorProfile) -> Self {
        // An empty table is the identity curve (`color::profiles::linear_rec2020_profile`).
        let linear = ToneReprCurve::Lut(Vec::new());
        profile.red_trc = Some(linear.clone());
        profile.green_trc = Some(linear.clone());
        profile.blue_trc = Some(linear);
        // moxcms believes a profile's CICP tag over its curves: `new_srgb()` carries sRGB's, and
        // with it left on, these linear curves converted as sRGB, a no-op into an sRGB display.
        profile.cicp = None;
        let display = match HdrDisplaySpace::for_host() {
            HdrDisplaySpace::LinearDisplayP3 => ColorProfile::new_display_p3(),
            HdrDisplaySpace::LinearSrgb => ColorProfile::new_srgb(),
        };
        // Luminance is the Y row of RGB to XYZ in the source's own white. ICC colorants are
        // Bradford-adapted to D50, which moves BT.2020's red weight from 0.2627 to 0.279, so the
        // adaptation is undone back to D65, every HDR source's white.
        let to_d65 = moxcms::adaption_matrix_d(
            moxcms::Chromaticity::D50.to_xyz(),
            moxcms::Chromaticity::D65.to_xyz(),
        );
        let rgb_to_xyz_d65 = to_d65.mat_mul(profile.colorant_matrix());
        Self {
            to_display: gain_map::to_mat3(profile.transform_matrix(&display)),
            weights: rgb_to_xyz_d65.v[1].map(|v| v as f32),
            linear: profile,
        }
    }

    fn params(&self, content_peak: f32) -> SceneLinearParams {
        SceneLinearParams {
            source_to_display: self.to_display,
            content_peak,
            luminance_weights: self.weights,
        }
    }
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoding::{PixelBuffer, PixelEncoding};
    use std::borrow::Cow;

    fn cicp(primaries: u8, transfer: u8, matrix: u8, full_range: bool) -> PngColor {
        classify(Some(png::CodingIndependentCodePoints {
            color_primaries: primaries,
            transfer_function: transfer,
            matrix_coefficients: matrix,
            is_video_full_range_image: full_range,
        }))
    }

    #[test]
    fn cicp_picks_the_path() {
        assert_eq!(classify(None), PngColor::Untagged);
        assert_eq!(
            cicp(9, 16, 0, true),
            PngColor::Hdr(HdrSignal {
                transfer: Transfer::Pq,
                primaries: 9,
                full_range: true
            })
        );
        assert_eq!(
            cicp(9, 18, 0, false),
            PngColor::Hdr(HdrSignal {
                transfer: Transfer::Hlg,
                primaries: 9,
                full_range: false
            })
        );
        // Display P3 with the sRGB curve: SDR, with the profile it names.
        let PngColor::Sdr { icc, full_range } = cicp(12, 13, 0, true) else {
            panic!("an SDR cICP gets its profile");
        };
        assert!(full_range);
        let profile = ColorProfile::new_from_slice(&icc).expect("the profile parses");
        let to_p3 = profile.transform_matrix(&ColorProfile::new_display_p3());
        assert!((to_p3.v[0][0] - 1.0).abs() < 1e-3, "{to_p3:?}");
        // PNG is RGB only; any matrix coefficients mean a broken chunk, and today's path.
        assert_eq!(cicp(9, 16, 1, true), PngColor::Untagged);
        // Unspecified primaries or transfer name no SDR profile.
        assert_eq!(cicp(2, 13, 0, true), PngColor::Untagged);
        assert_eq!(cicp(1, 2, 0, true), PngColor::Untagged);
    }

    #[test]
    fn narrow_range_expands_to_full_at_both_depths() {
        // 16-bit: black 4096, white 60160 (16 and 235 scaled by 256).
        let mut sixteen = vec![4096, 60160, 32128, 1000, 0, 65535, 4096, 4096];
        expand_narrow_range(&mut sixteen, 16);
        assert_eq!(&sixteen[..4], &[0, 65535, 32768, 1000], "alpha untouched");
        assert_eq!(&sixteen[4..7], &[0, 65535, 0], "out-of-range codes clamp");
        // 8-bit sources arrive widened by 257: 16 and 235 are black and white.
        let mut eight = vec![16 * 257, 235 * 257, 0, 255 * 257];
        expand_narrow_range(&mut eight, 8);
        assert_eq!(eight, [0, 65535, 0, 255 * 257]);
    }

    /// BT.2020's luminance weights, which the HLG OOTF names, from the colorants' D50 Y row.
    #[test]
    fn bt2020_luminance_weights_are_bt2100s() {
        let w = SourceSpace::from_cicp(9).weights;
        for (got, want) in w.iter().zip([0.2627, 0.6780, 0.0593]) {
            assert!((got - want).abs() < 0.001, "{w:?}");
        }
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }

    /// A 16-bit RGB PNG, optionally with `cICP` and an ICC profile.
    pub(crate) fn png_bytes(
        width: u32,
        height: u32,
        rgb16: &[u16],
        cicp: Option<[u8; 4]>,
        icc: Option<Vec<u8>>,
    ) -> Vec<u8> {
        let mut info = png::Info::with_size(width, height);
        info.bit_depth = png::BitDepth::Sixteen;
        info.color_type = png::ColorType::Rgb;
        info.icc_profile = icc.map(Cow::Owned);
        let mut out = Vec::new();
        let encoder = png::Encoder::with_info(&mut out, info).expect("encoder");
        let mut writer = encoder.write_header().expect("header");
        if let Some(cicp) = cicp {
            writer.write_chunk(png::chunk::cICP, &cicp).expect("cICP");
        }
        let data: Vec<u8> = rgb16.iter().flat_map(|v| v.to_be_bytes()).collect();
        writer.write_image_data(&data).expect("pixels");
        writer.finish().expect("finish");
        out
    }

    fn decode(bytes: Vec<u8>, target: &[u8], hdr_active: bool) -> DecodedImage {
        crate::decoding::generic::decode(
            std::path::Path::new("test.png"),
            bytes,
            target,
            false,
            OutputDepth::Native,
            hdr_active,
        )
        .expect("decodes")
    }

    /// What the byte consumers (clipboard, print, previews) get: the SDR rendition as RGBA8.
    fn decode_rgba8(bytes: Vec<u8>, target: &[u8]) -> DecodedImage {
        crate::decoding::generic::decode(
            std::path::Path::new("test.png"),
            bytes,
            target,
            false,
            OutputDepth::Rgba8,
            false,
        )
        .expect("decodes")
    }

    fn half(bits: u16) -> f32 {
        f16::from_bits(bits).to_f32()
    }

    fn grey_pq(codes: &[f32]) -> Vec<u16> {
        codes
            .iter()
            .flat_map(|&c| [(c * 65_535.0).round() as u16; 3])
            .collect()
    }

    /// PQ's reference values come out as light relative to SDR white, unclipped.
    #[test]
    fn a_pq_png_decodes_to_light_relative_to_sdr_white() {
        let codes = [0.0, 0.5806, 0.7518, 1.0];
        let bytes = png_bytes(4, 1, &grey_pq(&codes), Some([9, 16, 0, 1]), None);
        let image = decode(bytes, color::srgb_icc_bytes(), true);
        let PixelEncoding::SceneLinear(params) = image.encoding else {
            panic!("expected scene-linear, got {:?}", image.encoding);
        };
        let PixelBuffer::Rgba16F(halves) = &image.pixels else {
            unreachable!()
        };
        let got: Vec<f32> = halves
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| half(px[1]))
            .collect();
        for (got, want) in got.iter().zip([0.0, 1.0, 1000.0 / 203.0, 10_000.0 / 203.0]) {
            assert!((got - want).abs() <= want * 0.005, "{got:?} vs {want}");
        }
        // Three of four pixels at or under 1,000 nits: the 99.9th percentile is the brightest.
        assert!(
            (params.content_peak - 10_000.0 / 203.0).abs() < 0.5,
            "{params:?}"
        );
    }

    /// HLG's 75% signal is reference white.
    #[test]
    fn an_hlg_png_puts_reference_white_at_sdr_white() {
        let bytes = png_bytes(1, 1, &grey_pq(&[0.75]), Some([9, 18, 0, 1]), None);
        let image = decode(bytes, color::srgb_icc_bytes(), true);
        assert_eq!(image.encoding.kind().name(), "scene_linear");
        let PixelBuffer::Rgba16F(halves) = &image.pixels else {
            unreachable!()
        };
        assert!((half(halves[0]) - 1.0).abs() < 0.01, "{}", half(halves[0]));
    }

    /// The narrow-range flag reaches the signal: code 60160 is full white, so PQ's 10,000 nits.
    #[test]
    fn a_narrow_range_pq_png_is_scaled_to_full_range() {
        let bytes = png_bytes(1, 1, &[60160; 3], Some([9, 16, 0, 0]), None);
        let image = decode(bytes, color::srgb_icc_bytes(), true);
        let PixelBuffer::Rgba16F(halves) = &image.pixels else {
            unreachable!()
        };
        assert!(
            (half(halves[0]) - 10_000.0 / 203.0).abs() < 0.3,
            "{}",
            half(halves[0])
        );
    }

    /// cICP outranks iCCP (PNG third edition): a PQ PNG that also carries an sRGB profile is PQ.
    #[test]
    fn cicp_wins_over_an_icc_profile() {
        let srgb = color::srgb_icc_bytes().to_vec();
        let bytes = png_bytes(1, 1, &grey_pq(&[0.5806]), Some([9, 16, 0, 1]), Some(srgb));
        let image = decode(bytes, color::srgb_icc_bytes(), true);
        assert_eq!(image.encoding.kind().name(), "scene_linear");
    }

    /// An SDR cICP is honored: Display P3 code points decode exactly like the same samples
    /// tagged with a Display P3 ICC profile.
    #[test]
    fn an_sdr_cicp_png_decodes_like_its_icc_twin() {
        let red: Vec<u16> = [65_535, 0, 0, 40_000, 30_000, 20_000].to_vec();
        let p3 = ColorProfile::new_display_p3().encode().expect("P3 encodes");
        let target = color::srgb_icc_bytes();
        let by_cicp = decode(
            png_bytes(2, 1, &red, Some([12, 13, 0, 1]), None),
            target,
            true,
        );
        let by_icc = decode(png_bytes(2, 1, &red, None, Some(p3)), target, true);
        let untagged = decode(png_bytes(2, 1, &red, None, None), target, true);
        let halves = |i: &DecodedImage| match &i.pixels {
            PixelBuffer::Rgba16F(h) => h.iter().map(|&b| half(b)).collect::<Vec<_>>(),
            PixelBuffer::Rgba8(_) => panic!("a 16-bit PNG stays 16-bit"),
        };
        assert_eq!(by_cicp.encoding, PixelEncoding::DisplaySdr);
        for (a, b) in halves(&by_cicp).iter().zip(halves(&by_icc)) {
            assert!((a - b).abs() < 2e-3, "{a} vs {b}");
        }
        // And that's not what the same samples read as sRGB give: P3 red is out of sRGB.
        assert_ne!(halves(&by_cicp), halves(&untagged));
    }

    /// A saturated highlight keeps its hue on the SDR fallback: the primaries change first and
    /// the clip happens in the display's primaries, as the GPU path plus the compositor does. A
    /// clamp in BT.2020 before the matrix (moxcms' in-place f32 transform does that to its input)
    /// turns a PQ sunset's orange into salmon.
    #[test]
    fn the_sdr_fallback_clips_in_the_displays_primaries() {
        // A bright BT.2020 orange (luminance 1.14, so the tone map acts) and a saturated green
        // that lands outside sRGB, negative in red.
        let light = [[3.0f32, 0.5, 0.2], [0.05, 0.8, 0.05]];
        let samples: Vec<u16> = light
            .iter()
            .flatten()
            .map(|&v| (transfer::pq_inverse_eotf(v * 203.0) * 65_535.0).round() as u16)
            .collect();
        let bytes = png_bytes(2, 1, &samples, Some([9, 16, 0, 1]), None);
        let image = decode_rgba8(bytes, color::srgb_icc_bytes());
        let PixelBuffer::Rgba8(rgba) = &image.pixels else {
            panic!("the SDR fallback is RGBA8 at 8 bits");
        };

        // The reference: what decode measured as the peak (the brighter pixel), the tone map in
        // BT.2020, BT.2020 → sRGB unclipped, then the clip and the sRGB curve.
        let weights = SourceSpace::from_cicp(9).weights;
        let peak = light
            .iter()
            .map(|&px| luminance(px, weights))
            .fold(0.0f32, f32::max);
        let curve = tone_map::ToneCurve::new(peak, 1.0);
        let m = crate::color::profiles::linear_rec2020_profile()
            .transform_matrix(&ColorProfile::new_srgb())
            .v;
        let encode = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            let e = if v <= 0.003_130_8 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (e * 255.0).round() as u8
        };
        for (i, px) in light.iter().enumerate() {
            let toned = curve.rgb(*px, weights);
            let want: Vec<u8> = (0..3)
                .map(|r| encode((0..3).map(|c| m[r][c] as f32 * toned[c]).sum()))
                .collect();
            let got = &rgba[i * 4..i * 4 + 3];
            for (g, w) in got.iter().zip(&want) {
                assert!(g.abs_diff(*w) <= 1, "pixel {i}: {got:?} vs {want:?}");
            }
        }
    }

    /// The SDR fallback: a photo, not grey. Shadows keep their PQ value, reference white stays
    /// bright, and the brightest highlight lands on display white. Reading the PQ codes as sRGB
    /// (what Prvw did before) lifts every shadow.
    #[test]
    fn the_sdr_fallback_tone_maps_pq_into_a_photo() {
        // Black, a 1-nit shadow, 10 nits, reference white, and a 1,000-nit highlight.
        let nits = [0.0, 1.0, 10.0, 203.0, 1000.0];
        let codes: Vec<f32> = nits.iter().map(|&n| transfer::pq_inverse_eotf(n)).collect();
        let bytes = png_bytes(5, 1, &grey_pq(&codes), Some([9, 16, 0, 1]), None);
        let image = decode_rgba8(bytes, color::srgb_icc_bytes());
        let PixelBuffer::Rgba8(rgba) = &image.pixels else {
            panic!("the SDR fallback is RGBA8 at 8 bits");
        };
        assert_eq!(image.encoding, PixelEncoding::DisplaySdr);
        let got: Vec<u8> = rgba.as_chunks::<4>().0.iter().map(|px| px[1]).collect();
        // The reference, independently: BT.2390 onto SDR white, then the sRGB curve.
        let peak = 1000.0 / 203.0;
        let want: Vec<u8> = nits
            .iter()
            .map(|&n| {
                let y = tone_map::eetf(n / 203.0, peak, 1.0);
                let v = if y <= 0.003_130_8 {
                    12.92 * y
                } else {
                    1.055 * y.powf(1.0 / 2.4) - 0.055
                };
                (v.clamp(0.0, 1.0) * 255.0).round() as u8
            })
            .collect();
        for (g, w) in got.iter().zip(&want) {
            assert!(g.abs_diff(*w) <= 1, "{got:?} vs {want:?}");
        }
        assert_eq!(got[0], 0);
        assert_eq!(got[4], 255, "the content peak is display white");
        assert!(got[3] > 200, "reference white stays bright: {got:?}");
        // PQ codes read as sRGB (before M4) more than doubled every shadow.
        let naive = (codes[1] * 255.0).round() as u8;
        assert!(naive > got[1] * 2, "the old grey: {naive} vs {}", got[1]);
    }

    fn float_tiff(values: &[f32]) -> Vec<u8> {
        let rgb: Vec<f32> = values
            .iter()
            .flat_map(|&v| [v, v * 0.5, v * 0.25])
            .collect();
        let buffer =
            image::ImageBuffer::<image::Rgb<f32>, _>::from_raw(values.len() as u32, 1, rgb)
                .expect("dimensions");
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb32F(buffer)
            .write_to(&mut out, image::ImageFormat::Tiff)
            .expect("encode TIFF");
        out.into_inner()
    }

    fn decode_tiff(bytes: Vec<u8>, hdr_active: bool, depth: OutputDepth) -> DecodedImage {
        crate::decoding::generic::decode(
            std::path::Path::new("test.tif"),
            bytes,
            color::srgb_icc_bytes(),
            false,
            depth,
            hdr_active,
        )
        .expect("decodes")
    }

    /// A render's lamps or a star field: highlights on far fewer than 0.1% of the pixels, which
    /// the content-peak percentile rightly ignores, are still HDR, and must reach the EDR surface
    /// rather than clip at white.
    #[test]
    fn a_float_tiff_with_sparse_highlights_is_still_hdr() {
        // 4,000 pixels: two stars at 20× SDR white (0.05%) on a dim sky.
        let values: Vec<f32> = (0..4000)
            .map(|i| if i % 2000 == 7 { 20.0 } else { 0.02 })
            .collect();
        let image = decode_tiff(float_tiff(&values), true, OutputDepth::Native);
        assert_eq!(image.encoding.kind().name(), "scene_linear");
    }

    #[test]
    fn a_float_tiff_above_sdr_white_is_scene_linear() {
        let tiff = float_tiff(&[0.0, 0.5, 1.0, 2.0, 4.0]);
        let image = decode_tiff(tiff, true, OutputDepth::Native);
        let PixelEncoding::SceneLinear(params) = image.encoding else {
            panic!("expected scene-linear, got {:?}", image.encoding);
        };
        // The brightest pixel: 4.0 red, 2.0 green, 1.0 blue in BT.709.
        let want_y = 4.0 * params.luminance_weights[0]
            + 2.0 * params.luminance_weights[1]
            + params.luminance_weights[2];
        assert!((params.content_peak - want_y).abs() < 1e-3, "{params:?}");
        let PixelBuffer::Rgba16F(halves) = &image.pixels else {
            unreachable!()
        };
        assert_eq!(half(halves[16]), 4.0, "unclipped");
    }

    /// A float TIFF that never passes SDR white is SDR in a float container: M1's 16-bit shape,
    /// never the EDR surface, even on an HDR display.
    #[test]
    fn a_float_tiff_within_sdr_white_takes_the_16_bit_sdr_path() {
        let tiff = float_tiff(&[0.0, 0.18, 0.5, 1.0]);
        let image = decode_tiff(tiff, true, OutputDepth::Native);
        assert_eq!(image.encoding, PixelEncoding::DisplaySdr);
        let PixelBuffer::Rgba16F(halves) = &image.pixels else {
            panic!("expected half-floats");
        };
        // Linear in, linear out: the sRGB target's own curve and the surface's cancel, so the
        // stored half-float is the linear value itself (white is exactly 1.0).
        assert!(
            (half(halves[12]) - 1.0).abs() < 1e-3,
            "{}",
            half(halves[12])
        );
        assert!((half(halves[4]) - 0.18).abs() < 1e-3, "{}", half(halves[4]));
    }

    #[test]
    fn a_float_tiff_without_hdr_keeps_16_bits_and_byte_consumers_get_rgba8() {
        let viewer = decode_tiff(float_tiff(&[0.0, 1.0, 4.0]), false, OutputDepth::Native);
        assert!(matches!(viewer.pixels, PixelBuffer::Rgba16F(_)));
        assert_eq!(viewer.encoding, PixelEncoding::DisplaySdr);
        let bytes = decode_tiff(float_tiff(&[0.0, 1.0, 4.0]), true, OutputDepth::Rgba8);
        assert!(matches!(bytes.pixels, PixelBuffer::Rgba8(_)));
        assert_eq!(bytes.encoding, PixelEncoding::DisplaySdr);
    }

    /// A 16-bit PQ PNG on an SDR display: a strong tone curve then an 8-bit rounding is where
    /// skies band, so the viewer gets M1's 16-bit `DisplaySdr` shape, and only the byte
    /// consumers get RGBA8. Both carry the same rendition.
    #[test]
    fn a_pq_png_on_an_sdr_display_keeps_16_bits() {
        let codes: Vec<f32> = [0.0, 10.0, 203.0, 1000.0]
            .iter()
            .map(|&n| transfer::pq_inverse_eotf(n))
            .collect();
        let bytes = png_bytes(4, 1, &grey_pq(&codes), Some([9, 16, 0, 1]), None);
        let viewer = decode(bytes.clone(), color::srgb_icc_bytes(), false);
        assert_eq!(viewer.encoding, PixelEncoding::DisplaySdr);
        let PixelBuffer::Rgba16F(halves) = &viewer.pixels else {
            panic!("the viewer's SDR rendition keeps 16 bits");
        };
        let PixelBuffer::Rgba8(rgba) = &decode_rgba8(bytes, color::srgb_icc_bytes()).pixels else {
            panic!("byte consumers get RGBA8");
        };
        for (half_bits, byte) in halves.iter().zip(rgba) {
            let as_byte = color::display_sdr::display_sdr_f16_to_u8(*half_bits);
            assert!(as_byte.abs_diff(*byte) <= 1, "{as_byte} vs {byte}");
        }
    }

    /// The decode cost the spec budgets (section 9): our added work on a 12 MP PQ PNG (to light,
    /// peak histogram, half-float pack) under 25 ms. Release only:
    /// `RUST_LOG=prvw::decoding::scene_linear=debug cargo test --release pq_decode_perf -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn pq_decode_perf() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (width, height) = (4000u32, 3000u32);
        let samples: Vec<u16> = (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    let noise =
                        ((x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 22) & 0xff;
                    [
                        (x * 50_000 / width + noise) as u16,
                        (y * 50_000 / height + noise) as u16,
                        (20_000 + noise) as u16,
                    ]
                })
            })
            .collect();
        let bytes = png_bytes(width, height, &samples, Some([9, 16, 0, 1]), None);
        // The one thing every PNG now pays, HDR or not: the header probe.
        let untagged = png_bytes(width, height, &samples, None, None);
        let start = Instant::now();
        for _ in 0..100 {
            assert_eq!(probe_png(&untagged), PngColor::Untagged);
        }
        println!("cICP probe: {:.1} µs per PNG", ms(start.elapsed()) * 10.0);
        let img = image::load_from_memory(&bytes).expect("decodes");
        let signal = HdrSignal {
            transfer: Transfer::Pq,
            primaries: 9,
            full_range: true,
        };
        for transfer in [Transfer::Pq, Transfer::Hlg] {
            let signal = HdrSignal { transfer, ..signal };
            let _ = decode_hdr_png(
                img.clone(),
                signal,
                color::srgb_icc_bytes(),
                false,
                true,
                OutputDepth::Native,
            );
            let mut runs: Vec<(f64, f64)> = (0..7)
                .map(|_| {
                    let img = img.clone();
                    let start = Instant::now();
                    let widened = img.into_rgba16();
                    let to_rgba16 = ms(start.elapsed());
                    let start = Instant::now();
                    let _ = decode_hdr_png(
                        DynamicImage::ImageRgba16(widened),
                        signal,
                        color::srgb_icc_bytes(),
                        false,
                        true,
                        OutputDepth::Native,
                    );
                    (ms(start.elapsed()), to_rgba16)
                })
                .collect();
            runs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let (ours, widen) = runs[runs.len() / 2];
            println!(
                "{transfer:?} 12 MP: added work {ours:.1} ms (to_rgba16 of RGB16 {widen:.1} ms)"
            );
        }
        let start = Instant::now();
        let _ = decode(bytes.clone(), color::srgb_icc_bytes(), true);
        println!("whole PQ decode, HDR: {:.1} ms", ms(start.elapsed()));
        let start = Instant::now();
        let _ = decode(bytes, color::srgb_icc_bytes(), false);
        println!(
            "whole PQ decode, SDR fallback: {:.1} ms",
            ms(start.elapsed())
        );
    }
}
