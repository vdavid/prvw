//! CIE76 Delta-E for comparing two RGBA8 image buffers in perceptually uniform
//! Lab space. Used by RAW pipeline regression tests and the `raw-dev-dump`
//! example to check how far a new output drifts from a golden reference.
//!
//! `#[allow(dead_code)]` is set at the module level because the current
//! callers are the macOS-gated `synthetic_dng_matches_golden` test and the
//! `raw-dev-dump` example. Linux builds of the binary don't see any callers,
//! and Cargo's binary-crate layout doesn't treat example use as a callsite
//! for the main binary. Promote this gate away once the module has a
//! production caller (Phase 2.x will likely grow one).
#![allow(dead_code)]
//!
//! CIE76 is the original 1976 formula: Euclidean distance between the two
//! colors in Lab space. It's good enough for our regression needs (we care
//! about "is this drift visible?"). CIE2000 is more accurate but ~30× the
//! code and not worth it here.
//!
//! Reference ranges (rule of thumb for 8-bit sRGB):
//! - `< 1`: imperceptible to most people.
//! - `1-2`: barely perceptible on a side-by-side comparison.
//! - `2-10`: perceptible.
//! - `> 10`: clearly different.
//!
//! Alpha is ignored. Both buffers must have the same dimensions.
//!
//! The conversion chain is: sRGB 8-bit → linear RGB → XYZ (D65) → Lab (D65).

/// Aggregate Delta-E metrics between two RGBA8 images.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeltaEStats {
    /// Mean Delta-E across all pixels.
    pub mean: f32,
    /// Maximum Delta-E across all pixels.
    pub max: f32,
    /// 95th percentile Delta-E.
    pub p95: f32,
    /// Pixel count.
    pub count: usize,
}

/// Compute CIE76 Delta-E statistics between two RGBA8 buffers.
/// Panics if the lengths don't match or aren't divisible by 4.
pub fn delta_e_stats(a: &[u8], b: &[u8]) -> DeltaEStats {
    assert_eq!(a.len(), b.len(), "buffer lengths must match");
    assert_eq!(a.len() % 4, 0, "buffer length must be a multiple of 4");
    let count = a.len() / 4;
    if count == 0 {
        return DeltaEStats {
            mean: 0.0,
            max: 0.0,
            p95: 0.0,
            count: 0,
        };
    }

    let mut deltas: Vec<f32> = Vec::with_capacity(count);
    let mut sum = 0.0f64;
    let mut max = 0.0f32;
    for i in 0..count {
        let pa = [a[i * 4], a[i * 4 + 1], a[i * 4 + 2]];
        let pb = [b[i * 4], b[i * 4 + 1], b[i * 4 + 2]];
        let d = delta_e_cie76(pa, pb);
        deltas.push(d);
        sum += d as f64;
        if d > max {
            max = d;
        }
    }
    let mean = (sum / count as f64) as f32;
    // Simple p95: sort then index. Fine up to a few million pixels.
    deltas.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let p95_idx = ((count as f32) * 0.95).ceil() as usize;
    let p95 = deltas[p95_idx.min(count - 1)];

    DeltaEStats {
        mean,
        max,
        p95,
        count,
    }
}

/// CIE76 Delta-E between two sRGB 8-bit pixels. Returns Euclidean distance in
/// Lab space (D65).
pub fn delta_e_cie76(a: [u8; 3], b: [u8; 3]) -> f32 {
    let la = srgb8_to_lab(a);
    let lb = srgb8_to_lab(b);
    let dl = la[0] - lb[0];
    let da = la[1] - lb[1];
    let db = la[2] - lb[2];
    (dl * dl + da * da + db * db).sqrt()
}

/// sRGB 8-bit → CIE Lab (D65 white point).
fn srgb8_to_lab(p: [u8; 3]) -> [f32; 3] {
    let lin = [
        srgb_to_linear(p[0] as f32 / 255.0),
        srgb_to_linear(p[1] as f32 / 255.0),
        srgb_to_linear(p[2] as f32 / 255.0),
    ];
    let xyz = linear_rgb_to_xyz(lin);
    xyz_to_lab(xyz)
}

/// sRGB companding: inverse of the sRGB transfer function.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear sRGB → XYZ (D65). sRGB matrix from IEC 61966-2-1.
fn linear_rgb_to_xyz(rgb: [f32; 3]) -> [f32; 3] {
    let r = rgb[0];
    let g = rgb[1];
    let b = rgb[2];
    [
        0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b,
        0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b,
        0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b,
    ]
}

/// XYZ → Lab, D65 reference white (CIE 1931 2°).
fn xyz_to_lab(xyz: [f32; 3]) -> [f32; 3] {
    // D65 white point (normalized so Y = 1.0).
    const XN: f32 = 0.950_47;
    const YN: f32 = 1.0;
    const ZN: f32 = 1.088_83;
    let fx = lab_f(xyz[0] / XN);
    let fy = lab_f(xyz[1] / YN);
    let fz = lab_f(xyz[2] / ZN);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Lab non-linear segment.
fn lab_f(t: f32) -> f32 {
    // (6/29)^3
    const DELTA3: f32 = 0.008_856_452;
    // 1 / (3 * (6/29)^2) = 7.787...
    const K: f32 = 7.787_037;
    if t > DELTA3 {
        t.cbrt()
    } else {
        K * t + 16.0 / 116.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Identical pixels should give Delta-E = 0.
    #[test]
    fn identical_pixels_are_zero() {
        let d = delta_e_cie76([128, 64, 200], [128, 64, 200]);
        assert!(d < 0.001, "expected 0, got {d}");
    }

    /// Near-identical grays (off by one per channel) should give a small
    /// Delta-E below 1.0.
    #[test]
    fn near_identical_grays_below_one() {
        let d = delta_e_cie76([128, 128, 128], [129, 129, 129]);
        assert!(d < 1.0, "expected small delta, got {d}");
    }

    /// Saturated opposite colors should give a very large Delta-E. Red vs
    /// cyan is one of the classic worst-case pairs — expected around 110.
    #[test]
    fn saturated_opposites_are_large() {
        let d = delta_e_cie76([255, 0, 0], [0, 255, 255]);
        assert!(d > 100.0, "expected > 100, got {d}");
    }

    /// Black vs white: L* goes 0 → 100, a* and b* are both zero. Delta-E = 100.
    #[test]
    fn black_vs_white_is_one_hundred() {
        let d = delta_e_cie76([0, 0, 0], [255, 255, 255]);
        assert!((d - 100.0).abs() < 0.5, "expected ~100, got {d}");
    }

    /// Stats over a trivial two-pixel pair.
    #[test]
    fn stats_on_simple_buffers() {
        let a: Vec<u8> = vec![0, 0, 0, 255, 255, 255, 255, 255];
        let b: Vec<u8> = vec![0, 0, 0, 255, 255, 255, 255, 255];
        let s = delta_e_stats(&a, &b);
        assert_eq!(s.count, 2);
        assert!(s.mean < 0.001);
        assert!(s.max < 0.001);
        assert!(s.p95 < 0.001);
    }

    /// Stats over black+white vs white+black: each pixel has Delta-E ~100.
    #[test]
    fn stats_on_inverted_pair() {
        let a: Vec<u8> = vec![0, 0, 0, 255, 255, 255, 255, 255];
        let b: Vec<u8> = vec![255, 255, 255, 255, 0, 0, 0, 255];
        let s = delta_e_stats(&a, &b);
        assert_eq!(s.count, 2);
        assert!(s.mean > 99.0 && s.mean < 101.0, "mean was {}", s.mean);
        assert!(s.max > 99.0 && s.max < 101.0, "max was {}", s.max);
    }
}
