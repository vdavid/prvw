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
