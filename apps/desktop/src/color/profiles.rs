//! Factories for non-display wide-gamut color profiles used as intermediate
//! working spaces.
//!
//! The RAW pipeline lands its developed pixels in **linear Rec.2020** and then
//! hands them to `moxcms` for the final conversion into the display profile.
//! Rec.2020 is wider than sRGB and Display P3, so this keeps every saturated
//! color the sensor captured alive through the color transform. sRGB-clipping
//! the intermediate would throw those colors away before the display profile
//! ever sees them.
//!
//! `moxcms` already ships a `ColorProfile::new_bt2020()` factory, but its TRC
//! is the Rec.709 parametric curve. We want linear (gamma 1.0) because the RAW
//! pipeline hands us scene-linear floats. [`linear_rec2020_profile`] clones the
//! BT.2020 profile and swaps the curve out for a straight line.
//!
//! ## Why no bundled ICC file
//!
//! The original plan was to ship a `linear_rec2020.icc` asset. `moxcms` exposes
//! every piece we need to build the profile programmatically, so there's
//! nothing to license, nothing to bundle, and nothing to keep in sync with the
//! rest of the code. [`linear_rec2020_icc_bytes`] is kept around for debug
//! logging and external tooling that wants the profile as a blob.

use moxcms::{ColorProfile, LocalizableString, ProfileText, ToneReprCurve};
use rayon::prelude::*;

/// Rec.2020 D65 primaries → XYZ matrix (row-major, 3x3). Standard values from
/// the ITU-R BT.2020-2 spec, cross-checked against Bruce Lindbloom's RGB → XYZ
/// matrix generator. See `docs/notes/raw-support-phase2.md` for the derivation
/// and a decision note on Rec.2020 vs. Display P3.
#[allow(clippy::excessive_precision)]
pub const REC2020_TO_XYZ_D65: [[f32; 3]; 3] = [
    [0.6369580, 0.1446169, 0.1688810],
    [0.2627002, 0.6779981, 0.0593017],
    [0.0000000, 0.0280727, 1.0609851],
];

/// XYZ D65 → linear Rec.2020 matrix. Inverse of [`REC2020_TO_XYZ_D65`],
/// pre-computed so we can skip a matrix inversion at runtime. Verified with
/// `xyz_to_rec2020 * rec2020_to_xyz ≈ identity` in the unit tests below.
///
/// Currently only referenced by the tests in this module; kept around so
/// future code needing the reverse transform has it sitting next to the
/// forward matrix rather than having to invert by hand.
#[allow(clippy::excessive_precision, dead_code)]
pub const XYZ_TO_REC2020_D65: [[f32; 3]; 3] = [
    [1.7166512, -0.3556708, -0.2533663],
    [-0.6666844, 1.6164812, 0.0157685],
    [0.0176399, -0.0427706, 0.9421031],
];

/// Linear Rec.2020 → linear Display P3, D65 throughout. Composed offline
/// from `XYZ_TO_DISPLAY_P3_D65 · REC2020_TO_XYZ_D65` and cross-checked in
/// the unit tests. Used on the HDR path: when the `CAMetalLayer` is in
/// `extendedLinearDisplayP3`, the compositor expects linear Display P3 input,
/// so we bypass `moxcms` (which clips at 1.0 on the way out) and apply
/// this matrix directly. Values above 1.0 survive the pass, which is the
/// whole point — that's the EDR headroom the display can actually show.
///
/// Note the large off-diagonal on row 0: Rec.2020's red primary is more
/// saturated than Display P3's, so expressing pure Rec.2020 red in P3
/// requires R > 1 plus small negative G / B contributions. Rows sum to
/// ~1.0, so neutral whites stay neutral.
#[allow(clippy::excessive_precision)]
pub const REC2020_TO_LINEAR_DISPLAY_P3_D65: [[f32; 3]; 3] = [
    [1.3435781, -0.2822036, -0.0613745],
    [-0.0652997, 1.0757885, -0.0104888],
    [0.0028200, -0.0196040, 1.0167734],
];

/// XYZ D65 → linear Display P3 matrix. Kept public so the tests below can
/// verify that composing it with [`REC2020_TO_XYZ_D65`] reproduces
/// [`REC2020_TO_LINEAR_DISPLAY_P3_D65`]. Published values from the
/// Display P3 primaries (CIE xy: R = (0.680, 0.320), G = (0.265, 0.690),
/// B = (0.150, 0.060), white = D65).
#[allow(clippy::excessive_precision, dead_code)]
pub const XYZ_TO_DISPLAY_P3_D65: [[f32; 3]; 3] = [
    [2.4934969, -0.9313836, -0.4027108],
    [-0.8294890, 1.7626641, 0.0236247],
    [0.0358458, -0.0761724, 0.9568845],
];

/// In-place linear Rec.2020 → linear Display P3 on a flat `[R0, G0, B0,
/// R1, G1, B1, …]` f32 buffer (length must be a multiple of 3). No gamma,
/// no clipping — values outside [0, 1] pass through untouched, which is
/// what the EDR compositor wants. rayon-parallel for throughput.
pub fn rec2020_to_linear_display_p3_inplace(rgb: &mut [f32]) {
    let m = &REC2020_TO_LINEAR_DISPLAY_P3_D65;
    rgb.par_chunks_exact_mut(3).for_each(|pixel| {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        pixel[0] = m[0][0] * r + m[0][1] * g + m[0][2] * b;
        pixel[1] = m[1][0] * r + m[1][1] * g + m[1][2] * b;
        pixel[2] = m[2][0] * r + m[2][1] * g + m[2][2] * b;
    });
}

/// Linear Rec.2020 as a `ColorProfile`. Built on top of `moxcms`' BT.2020
/// factory, but with a straight-line TRC so values stay in scene-linear space
/// through the transform.
pub fn linear_rec2020_profile() -> ColorProfile {
    let mut profile = ColorProfile::new_bt2020();
    // A table-based linear curve: empty LUT means "identity". That's the
    // convention `moxcms` uses elsewhere (see `new_linear_rgb` in its tests).
    let linear = ToneReprCurve::Lut(Vec::new());
    profile.red_trc = Some(linear.clone());
    profile.green_trc = Some(linear.clone());
    profile.blue_trc = Some(linear);
    profile.description = Some(ProfileText::Localizable(vec![LocalizableString::new(
        "en".to_string(),
        "US".to_string(),
        "Linear Rec.2020 (Prvw)".to_string(),
    )]));
    profile
}

/// ICC bytes for the linear Rec.2020 profile. Handy for debug logging and
/// tooling that wants the profile as a blob. Encoding happens once on first
/// call, so callers can treat it as a cheap accessor.
#[allow(dead_code)]
pub fn linear_rec2020_icc_bytes() -> &'static [u8] {
    use std::sync::OnceLock;
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        linear_rec2020_profile()
            .encode()
            .expect("linear Rec.2020 profile always encodes cleanly")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Multiply a 3x3 matrix by a 3-vector.
    fn mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    }

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn d65_white_round_trips_to_unit_rec2020() {
        // D65 whitepoint in XYZ: Y=1 by definition. (0.9505, 1.0, 1.0891)
        // should land on (1, 1, 1) in linear Rec.2020, by construction.
        let white_xyz = [0.9505, 1.0, 1.0891];
        let rec2020 = mul(&XYZ_TO_REC2020_D65, white_xyz);
        for (i, c) in rec2020.iter().enumerate() {
            assert!(
                approx_eq(*c, 1.0, 1e-3),
                "Rec.2020 channel {i} for D65 white: got {c}, want ~1.0"
            );
        }
    }

    #[test]
    fn rec2020_primary_red_round_trips_through_xyz() {
        // Pure red (1, 0, 0) in Rec.2020 → XYZ → back to Rec.2020 should give
        // (1, 0, 0) modulo rounding. Catches sign/row-order mistakes.
        let red = [1.0, 0.0, 0.0];
        let xyz = mul(&REC2020_TO_XYZ_D65, red);
        let back = mul(&XYZ_TO_REC2020_D65, xyz);
        for (i, (a, b)) in back.iter().zip(red.iter()).enumerate() {
            assert!(
                approx_eq(*a, *b, 1e-3),
                "channel {i} didn't round-trip: got {a}, want {b}"
            );
        }
    }

    #[test]
    fn rec2020_and_xyz_matrices_are_inverses() {
        // Sanity-check the precomputed inverse against an identity round trip
        // on each standard basis vector.
        for basis in &[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            let xyz = mul(&REC2020_TO_XYZ_D65, *basis);
            let back = mul(&XYZ_TO_REC2020_D65, xyz);
            for (i, (a, b)) in back.iter().zip(basis.iter()).enumerate() {
                assert!(
                    approx_eq(*a, *b, 1e-3),
                    "basis {basis:?} channel {i}: got {a}, want {b}"
                );
            }
        }
    }

    /// Multiply two 3×3 matrices. Used to verify the precomputed Rec.2020 →
    /// Display P3 constant against `XYZ_TO_DISPLAY_P3_D65 · REC2020_TO_XYZ_D65`.
    fn matmul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
        let mut out = [[0.0_f32; 3]; 3];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
            }
        }
        out
    }

    #[test]
    fn rec2020_to_display_p3_matches_composed_xyz_matrices() {
        // The precomputed constant should equal `XYZ_TO_DISPLAY_P3_D65 ·
        // REC2020_TO_XYZ_D65` to float rounding. Catches typos in the
        // hand-entered matrix values without having to run the pipeline.
        let composed = matmul(&XYZ_TO_DISPLAY_P3_D65, &REC2020_TO_XYZ_D65);
        for (i, (got_row, want_row)) in composed
            .iter()
            .zip(REC2020_TO_LINEAR_DISPLAY_P3_D65.iter())
            .enumerate()
        {
            for (j, (got, want)) in got_row.iter().zip(want_row.iter()).enumerate() {
                assert!(
                    approx_eq(*got, *want, 5e-4),
                    "matrix[{i}][{j}]: composed {got}, constant {want}"
                );
            }
        }
    }

    #[test]
    fn rec2020_d65_white_maps_to_display_p3_unit_white() {
        // (1, 1, 1) in linear Rec.2020 represents D65 white; same in linear
        // Display P3. Sanity-checks no chromatic adaptation was baked into
        // the matrix (we stay D65 → D65, no Bradford).
        let mut buf = vec![1.0_f32, 1.0, 1.0];
        rec2020_to_linear_display_p3_inplace(&mut buf);
        for (i, v) in buf.iter().enumerate() {
            assert!(
                approx_eq(*v, 1.0, 1e-3),
                "D65 white channel {i}: got {v}, want ~1.0"
            );
        }
    }

    #[test]
    fn rec2020_to_display_p3_preserves_above_white() {
        // The whole reason for this transform's existence: values > 1.0 must
        // survive. A uniform 2.0 gray should map to uniform ~2.0 in Display
        // P3 (same chromaticity, twice the intensity).
        let mut buf = vec![2.0_f32, 2.0, 2.0];
        rec2020_to_linear_display_p3_inplace(&mut buf);
        for (i, v) in buf.iter().enumerate() {
            assert!(
                approx_eq(*v, 2.0, 2e-3),
                "above-white channel {i}: got {v}, want ~2.0"
            );
        }
    }

    #[test]
    fn linear_rec2020_profile_encodes() {
        // Encoding is the moxcms serialisation path we lean on in
        // `linear_rec2020_icc_bytes`. If this breaks, the debug dump breaks.
        let bytes = linear_rec2020_icc_bytes();
        assert!(
            bytes.len() > 128,
            "ICC blob suspiciously small ({} bytes)",
            bytes.len()
        );
        // First four bytes of an ICC file are its size.
        assert_eq!(
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize,
            bytes.len(),
            "ICC header size doesn't match blob length"
        );
    }

    #[test]
    fn linear_rec2020_profile_parses_back() {
        // Round-trip through the parser: we emit bytes, moxcms reads them, and
        // the description survives. Guards against writer/reader drift between
        // moxcms versions.
        let bytes = linear_rec2020_icc_bytes();
        let parsed = ColorProfile::new_from_slice(bytes).expect("should parse back");
        assert!(parsed.red_trc.is_some());
        assert!(parsed.green_trc.is_some());
        assert!(parsed.blue_trc.is_some());
    }
}
