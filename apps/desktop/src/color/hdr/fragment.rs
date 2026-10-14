//! The Rust reference for the image fragment shader's colour math (`render/shader.wgsl`), one
//! branch per [`ShaderMode`].
//!
//! It's the spec the WGSL is ported from, what the unit tests exercise, and what the GPU parity
//! test (`render::image_pipeline`'s `gpu_parity` tests) compares a real render against. Change
//! one and the parity test says whether the other followed.
//!
//! Input is the texture sample as the shader sees it: after the texture format's own decode, so
//! an `Rgba8UnormSrgb` texel arrives linearized and an `Rgba16Float` one as stored. Output is the
//! colour the shader writes before the checkerboard composite and the crossfade, both of which
//! are mode-independent.

use super::gain_map::{self, GainMapParams, MapView, Mat3};
use super::tone_map::{self, SceneLinearParams};

/// Which branch of the fragment shader draws an image. One per `PixelEncoding` kind, and the
/// discriminant is the `mode` field of the shader's `ImageParams` uniform, so the two must agree
/// (`render/shader.wgsl` names the same numbers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ShaderMode {
    /// `PixelEncoding::DisplaySdr`: display-encoded SDR, either storage. Passthrough.
    DisplaySdr = 0,
    /// `PixelEncoding::LinearHdrDisplay`: linear light in the platform's HDR display space, RAW
    /// HDR. Passthrough; values above 1.0 are the highlights.
    LinearHdrDisplay = 1,
    /// `PixelEncoding::GainMapped`: a linear Display P3 SDR base plus a gain map, weighted by the
    /// display's current headroom.
    GainMap = 2,
    /// `PixelEncoding::SceneLinear`: linear light in the source primaries (PQ and HLG PNGs,
    /// float TIFFs), tone-mapped from its content peak down to the display's current headroom.
    SceneLinear = 3,
}

/// What the shader reads from its per-frame uniform (group 1). Display-dependent, shared by every
/// image on screen.
// Only the tests and the GPU parity harness run the reference. The CPU SDR fallback for
// scene-linear images (`decoding::scene_linear`) calls `tone_map` directly instead: it has no
// texture sample or display matrix, and converts into the display profile with `moxcms`.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInputs {
    /// The display's current headroom over SDR white, at least 1.0.
    pub headroom_now: f32,
}

/// Everything mode 2 reads besides the base sample: the map (binding 3) and the per-image
/// parameters (binding 4).
#[derive(Clone, Copy, Debug)]
pub struct GainMapInputs<'a> {
    pub params: GainMapParams,
    pub map: MapView<'a>,
    /// The base image's size in texels, which the shader reads off its texture.
    pub image_size: [u32; 2],
    /// Linear Display P3 (the base as sampled) into the space the gain is applied in.
    pub p3_to_app: Mat3,
    /// That space into the platform's HDR display space.
    pub app_to_display: Mat3,
    /// Linear Display P3 straight into the HDR display space, for when no gain applies.
    pub p3_to_display: Mat3,
}

/// One image's per-image shader inputs, by mode.
// A borrowed view built per call and never stored, so the mode-2 variant's size costs nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug)]
pub enum ImageInputs<'a> {
    DisplaySdr,
    LinearHdrDisplay,
    GainMap(GainMapInputs<'a>),
    SceneLinear(SceneLinearParams),
}

impl ImageInputs<'_> {
    #[cfg_attr(not(test), allow(dead_code))] // See `FrameInputs`.
    pub fn mode(&self) -> ShaderMode {
        match self {
            ImageInputs::DisplaySdr => ShaderMode::DisplaySdr,
            ImageInputs::LinearHdrDisplay => ShaderMode::LinearHdrDisplay,
            ImageInputs::GainMap(_) => ShaderMode::GainMap,
            ImageInputs::SceneLinear(_) => ShaderMode::SceneLinear,
        }
    }
}

/// The colour the shader produces for one texel sample at texture coordinate `uv`.
#[cfg_attr(not(test), allow(dead_code))] // See `FrameInputs`.
pub fn shade(image: &ImageInputs, sample: [f32; 3], uv: [f32; 2], frame: FrameInputs) -> [f32; 3] {
    match image {
        // Neither passthrough mode reads the headroom: SDR is already display-encoded, and RAW
        // HDR is developed against a fixed 4× shoulder.
        ImageInputs::DisplaySdr | ImageInputs::LinearHdrDisplay => sample,
        ImageInputs::GainMap(g) => {
            let w = gain_map::weight(frame.headroom_now, &g.params);
            // No gain at all: show the base rendition as it is. ISO 21496-1 says so for a
            // headroom at or below the base's, and it keeps the formula's offsets (which needn't
            // cancel) from nudging the SDR rendition.
            if w == 0.0 {
                return gain_map::mul(&g.p3_to_display, sample);
            }
            let x = gain_map::mul(&g.p3_to_app, sample);
            let gain = gain_map::sample_gain(&g.map, g.image_size, uv, &g.params, w);
            gain_map::mul(&g.app_to_display, gain_map::apply(x, gain, &g.params))
        }
        // Tone-mapped in the source primaries (a uniform scale, so it commutes with the matrix),
        // then into the display space.
        ImageInputs::SceneLinear(s) => {
            let mapped = tone_map::tone_map(
                sample,
                s.luminance_weights,
                s.content_peak,
                frame.headroom_now.max(1.0),
            );
            gain_map::mul(&s.source_to_display, mapped)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::hdr::gain_map::{ChannelParams, IDENTITY};

    const FRAMES: [FrameInputs; 3] = [
        FrameInputs { headroom_now: 1.0 },
        FrameInputs { headroom_now: 2.5 },
        FrameInputs { headroom_now: 16.0 },
    ];
    const UV: [f32; 2] = [0.5, 0.5];

    #[test]
    fn display_sdr_passes_through_at_every_headroom() {
        for frame in FRAMES {
            assert_eq!(
                shade(&ImageInputs::DisplaySdr, [0.2, 0.5, 1.0], UV, frame),
                [0.2, 0.5, 1.0]
            );
        }
    }

    /// RAW HDR's highlights reach the surface untouched: the develop already put them where it
    /// wants them, and a brightness change must not move them.
    #[test]
    fn linear_hdr_display_keeps_values_above_white_at_every_headroom() {
        for frame in FRAMES {
            assert_eq!(
                shade(&ImageInputs::LinearHdrDisplay, [4.0, 1.0, 0.001], UV, frame),
                [4.0, 1.0, 0.001]
            );
        }
    }

    fn gain_mapped(rgba: &[u8]) -> ImageInputs<'_> {
        ImageInputs::GainMap(GainMapInputs {
            params: GainMapParams {
                channels: [ChannelParams {
                    max: 2.0,
                    ..ChannelParams::default()
                }; 3],
                base_headroom: 0.0,
                alternate_headroom: 2.0,
                use_base_color_space: true,
                curve: crate::color::hdr::gain_map::GainCurve::Log2,
            },
            map: MapView {
                width: 1,
                height: 1,
                rgba,
            },
            image_size: [4, 4],
            p3_to_app: IDENTITY,
            app_to_display: IDENTITY,
            p3_to_display: IDENTITY,
        })
    }

    /// At headroom 1 the SDR base comes out exactly as sampled, offsets and all.
    #[test]
    fn a_gain_map_at_headroom_1_is_exactly_the_sdr_base() {
        let image = gain_mapped(&[255, 255, 255, 255]);
        for sample in [[0.0, 0.0, 0.0], [0.2, 0.5, 1.0], [1.0, 1.0, 1.0]] {
            assert_eq!(
                shade(&image, sample, UV, FrameInputs { headroom_now: 1.0 }),
                sample
            );
        }
    }

    /// A full map value is the full 2 stops once the display has the headroom, and half of them
    /// (in log2) halfway there. White goes above 1.0, which is the point.
    #[test]
    fn a_full_gain_brightens_by_the_headroom_it_has() {
        let image = gain_mapped(&[255, 255, 255, 255]);
        let k = 1.0 / 64.0;
        for (headroom, gain) in [(4.0f32, 4.0f32), (16.0, 4.0), (2.0, 2.0)] {
            let got = shade(
                &image,
                [1.0; 3],
                UV,
                FrameInputs {
                    headroom_now: headroom,
                },
            );
            let want = (1.0 + k) * gain - k;
            assert!(
                got.iter().all(|v| (v - want).abs() < 1e-5),
                "headroom {headroom}: {got:?} vs {want}"
            );
        }
    }

    /// The numbers are the shader's contract: `shader.wgsl` switches on them.
    #[test]
    fn mode_numbers_match_the_shader() {
        assert_eq!(ShaderMode::DisplaySdr as u32, 0);
        assert_eq!(ShaderMode::LinearHdrDisplay as u32, 1);
        assert_eq!(ShaderMode::GainMap as u32, 2);
        assert_eq!(ShaderMode::SceneLinear as u32, 3);
    }

    const BT2020_WEIGHTS: [f32; 3] = [0.2627, 0.6780, 0.0593];

    fn scene_linear(content_peak: f32) -> ImageInputs<'static> {
        ImageInputs::SceneLinear(SceneLinearParams {
            source_to_display: [[1.5, -0.4, -0.1], [-0.1, 1.1, 0.0], [0.0, -0.1, 1.1]],
            content_peak,
            luminance_weights: BT2020_WEIGHTS,
        })
    }

    /// Content that fits the headroom only changes primaries: nothing is tone-mapped.
    #[test]
    fn scene_linear_content_that_fits_only_changes_primaries() {
        let image = scene_linear(3.0);
        let sample = [2.0, 1.0, 0.5];
        let got = shade(&image, sample, UV, FrameInputs { headroom_now: 4.0 });
        let ImageInputs::SceneLinear(p) = image else {
            unreachable!()
        };
        assert_eq!(got, gain_map::mul(&p.source_to_display, sample));
    }

    /// Brighter content than the display has is brought down to its headroom, and the headroom
    /// the shader sees is never below SDR white.
    #[test]
    fn scene_linear_content_peak_lands_on_the_headroom() {
        let identity = ImageInputs::SceneLinear(SceneLinearParams {
            source_to_display: gain_map::IDENTITY,
            content_peak: 8.0,
            luminance_weights: BT2020_WEIGHTS,
        });
        for (headroom, want) in [(2.0f32, 2.0f32), (1.0, 1.0), (0.5, 1.0)] {
            let got = shade(
                &identity,
                [8.0; 3],
                UV,
                FrameInputs {
                    headroom_now: headroom,
                },
            );
            assert!(
                got.iter().all(|v| (v - want).abs() < 1e-3 * want),
                "headroom {headroom}: {got:?}"
            );
        }
    }
}
