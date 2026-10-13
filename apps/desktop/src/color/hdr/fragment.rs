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
    // 2: gain map (M3). 3: scene-linear tone map (M4).
}

/// What the shader reads from its per-frame uniform (group 1). Display-dependent, shared by every
/// image on screen.
// Only the tests and the GPU parity harness run the reference until M4's CPU SDR fallback for
// PQ/HLG runs it for real.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameInputs {
    /// The display's current headroom over SDR white, at least 1.0.
    pub headroom_now: f32,
}

/// The colour the shader produces for one texel sample.
#[cfg_attr(not(test), allow(dead_code))] // See `FrameInputs`.
pub fn shade(mode: ShaderMode, sample: [f32; 3], frame: FrameInputs) -> [f32; 3] {
    // Neither passthrough mode reads the headroom: SDR is already display-encoded, and RAW HDR is
    // developed against a fixed 4× shoulder. The gain-map and tone-map modes are what will.
    let _ = frame;
    match mode {
        ShaderMode::DisplaySdr | ShaderMode::LinearHdrDisplay => sample,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAMES: [FrameInputs; 3] = [
        FrameInputs { headroom_now: 1.0 },
        FrameInputs { headroom_now: 2.5 },
        FrameInputs { headroom_now: 16.0 },
    ];

    #[test]
    fn display_sdr_passes_through_at_every_headroom() {
        for frame in FRAMES {
            assert_eq!(
                shade(ShaderMode::DisplaySdr, [0.2, 0.5, 1.0], frame),
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
                shade(ShaderMode::LinearHdrDisplay, [4.0, 1.0, 0.001], frame),
                [4.0, 1.0, 0.001]
            );
        }
    }

    /// The numbers are the shader's contract: `shader.wgsl` switches on them.
    #[test]
    fn mode_numbers_match_the_shader() {
        assert_eq!(ShaderMode::DisplaySdr as u32, 0);
        assert_eq!(ShaderMode::LinearHdrDisplay as u32, 1);
    }
}
