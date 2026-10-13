//! The half-float form of display-encoded SDR: what `PixelEncoding::DisplaySdr`
//! means when it's stored as `PixelBuffer::Rgba16F`.
//!
//! A 16-bit source is color-managed into the display profile at 16 bits, then
//! each display-encoded value goes through the sRGB EOTF into an `f16`. The
//! renderer uploads that as an `Rgba16Float` texture and draws it onto the same
//! `Bgra8UnormSrgb` surface as an 8-bit image; the surface's sRGB encode undoes
//! the EOTF, so the display-encoded value arrives intact and gets rounded to 8
//! bits exactly once. That's the same arithmetic an `Rgba8UnormSrgb` texture
//! goes through, minus the second rounding a 16-bit source used to pay.
//!
//! The EOTF here is the sRGB curve whatever the display profile's own curve is,
//! on purpose: it's the inverse of the surface's encode, not a color
//! conversion. f16 keeps about 11 significant bits, densest near black, which is
//! where the EOTF puts the most values.
//!
//! Both directions are 64K-entry lookup tables built once, since the input is
//! always a 16-bit pattern.

use std::sync::OnceLock;

use half::f16;
use rayon::prelude::*;

/// Below this many samples, a single loop beats splitting across rayon.
const PARALLEL_THRESHOLD: usize = 1 << 20;

/// sRGB EOTF: display-encoded `[0, 1]` → linear `[0, 1]`.
fn srgb_eotf(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB OETF, the inverse of [`srgb_eotf`].
fn srgb_oetf(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// 16-bit display-encoded value → `f16` bits of its linearized value.
fn eotf_table() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=u16::MAX)
            .map(|v| f16::from_f32(srgb_eotf(f32::from(v) / 65_535.0)).to_bits())
            .collect()
    })
}

/// `f16` bits of a linearized value → the 8-bit display-encoded value the
/// surface would write for it. Clamped to `[0, 1]`; NaN reads as 0.
fn to_u8_table() -> &'static [u8] {
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=u16::MAX)
            .map(|bits| {
                let linear = f16::from_bits(bits).to_f32();
                let linear = if linear.is_nan() {
                    0.0
                } else {
                    linear.clamp(0.0, 1.0)
                };
                (srgb_oetf(linear) * 255.0).round() as u8
            })
            .collect()
    })
}

/// Turn a display-encoded RGBA16 buffer into its `DisplaySdr` half-float form,
/// in place: color channels through the sRGB EOTF, alpha straight to `[0, 1]`
/// (alpha is linear coverage, never encoded).
pub fn rgba16_to_display_sdr_f16_inplace(rgba: &mut [u16]) {
    let eotf = eotf_table();
    let convert = |chunk: &mut [u16]| {
        for px in chunk.as_chunks_mut::<4>().0 {
            px[0] = eotf[px[0] as usize];
            px[1] = eotf[px[1] as usize];
            px[2] = eotf[px[2] as usize];
            px[3] = f16::from_f32(f32::from(px[3]) / 65_535.0).to_bits();
        }
    };
    if rgba.len() >= PARALLEL_THRESHOLD {
        // Whole pixels per chunk, so no pixel straddles two.
        rgba.par_chunks_mut(PARALLEL_THRESHOLD / 4 * 4)
            .for_each(convert);
    } else {
        convert(rgba);
    }
}

/// One `DisplaySdr` half-float color sample as the 8-bit display-encoded value
/// it stands for: what the screen gets, and what a byte consumer should get.
pub fn display_sdr_f16_to_u8(bits: u16) -> u8 {
    to_u8_table()[bits as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_and_white_land_on_the_ends() {
        let mut px = [0, 0, 0, 0, 65_535, 65_535, 65_535, 65_535];
        rgba16_to_display_sdr_f16_inplace(&mut px);
        for &bits in &px[..4] {
            assert_eq!(f16::from_bits(bits).to_f32(), 0.0);
        }
        for &bits in &px[4..] {
            assert_eq!(f16::from_bits(bits).to_f32(), 1.0);
        }
    }

    /// The sRGB EOTF puts mid-grey (display-encoded 0.5) at linear ~0.214.
    #[test]
    fn mid_grey_is_linearized() {
        let mut px = [32_768, 32_768, 32_768, 32_768];
        rgba16_to_display_sdr_f16_inplace(&mut px);
        let linear = f16::from_bits(px[0]).to_f32();
        assert!((linear - 0.214).abs() < 0.001, "got {linear}");
        let alpha = f16::from_bits(px[3]).to_f32();
        assert!(
            (alpha - 0.5).abs() < 0.001,
            "alpha stays linear, got {alpha}"
        );
    }

    /// Every 8-bit level survives the round trip: widened to 16 bits,
    /// linearized into f16, and read back as a byte, it's the byte it was.
    /// This is the promise the renderer's sRGB surface encode depends on.
    #[test]
    fn every_8_bit_level_round_trips() {
        for v in 0..=255u16 {
            let mut px = [v * 257, v * 257, v * 257, 65_535];
            rgba16_to_display_sdr_f16_inplace(&mut px);
            assert_eq!(display_sdr_f16_to_u8(px[0]), v as u8, "level {v}");
        }
    }

    /// The 16-bit precision the encoding exists for: f16 keeps far more than
    /// 256 distinct levels across a full 16-bit ramp.
    #[test]
    fn a_16_bit_ramp_keeps_thousands_of_levels() {
        let mut ramp: Vec<u16> = (0..=u16::MAX).flat_map(|v| [v, v, v, 65_535]).collect();
        rgba16_to_display_sdr_f16_inplace(&mut ramp);
        let mut levels: Vec<u16> = ramp.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
        levels.dedup();
        assert!(levels.len() > 10_000, "only {} levels", levels.len());
    }

    #[test]
    fn out_of_range_and_nan_clamp_when_read_as_bytes() {
        assert_eq!(display_sdr_f16_to_u8(f16::from_f32(1.5).to_bits()), 255);
        assert_eq!(display_sdr_f16_to_u8(f16::from_f32(-0.25).to_bits()), 0);
        assert_eq!(display_sdr_f16_to_u8(f16::NAN.to_bits()), 0);
    }

    #[test]
    fn the_parallel_path_matches_the_serial_one() {
        let len = PARALLEL_THRESHOLD * 2 + 4;
        let source: Vec<u16> = (0..len).map(|i| (i * 7919 % 65_536) as u16).collect();
        let mut parallel = source.clone();
        rgba16_to_display_sdr_f16_inplace(&mut parallel);
        let mut serial = source;
        for chunk in serial.chunks_mut(4096) {
            rgba16_to_display_sdr_f16_inplace(chunk);
        }
        assert_eq!(parallel, serial);
    }
}
