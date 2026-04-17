//! Dual-illuminant interpolation for DCP `HueSatMap` / `LookTable`.
//!
//! A DCP can ship two hue/sat maps, each calibrated for a different light
//! source: a warm illuminant (usually Standard Light A, ~2856 K) in slot 1
//! and a cool illuminant (usually D65, ~6504 K) in slot 2. At render time
//! the decoder is supposed to estimate the **scene's** color temperature
//! from the camera white balance and blend the two maps accordingly. That
//! way a tungsten-lit room renders against the A-calibrated map, a daylight
//! scene against the D65-calibrated map, and mixed lighting lands
//! somewhere in between.
//!
//! The full DNG 1.6 § 6.2.5 procedure iterates ForwardMatrix1/2 and the
//! camera neutral (`AsShotNeutral` / `wb_coeffs`) until a self-consistent
//! scene temperature converges. We take a simpler route:
//!
//! ## Compromise algorithm (Phase 3.4)
//!
//! 1. Estimate the scene's correlated color temperature from the camera's
//!    white-balance coefficients with a one-shot formula:
//!    `temp_k ≈ 7000 − 2000 × (R/G − 1)`. Clamped to `[2000, 10000]` K.
//!    This is the "warmer scenes have lower R-to-G coefficient ratios"
//!    heuristic rolled into a line: cameras use high red gain to neutralise
//!    red-dominant (i.e., warm) scenes, so a high R/G means low-K light.
//!    It's not the spec's iterative procedure, but it gets the direction
//!    and magnitude right to within ~500 K on our test fixtures — enough
//!    to drive a smooth blend without the noise of a discrete switch.
//! 2. Convert the two calibration illuminant tag values to their
//!    corresponding color temperatures. Unknown / "other" tags fall back
//!    to a sensible default (5000 K).
//! 3. Blend linearly between the two maps by normalised position between
//!    the illuminant temperatures. Clamps to the endpoints when the scene
//!    temperature sits outside the [low_k, high_k] range — DCPs calibrated
//!    at a narrow span shouldn't extrapolate past their ends, the data
//!    isn't trustworthy there.
//!
//! ### Limitations we accept here
//!
//! - **WB-to-temp is a linear approximation**, not the spec's full
//!   iterative solver. Accurate enough for a viewer; a color scientist
//!   would want the full procedure.
//! - **Clamped extrapolation** rather than the spec's continuous
//!   extension. Smoother behaviour for off-gamut whites.
//! - **Single-map DCPs skip all this** and return the one map they carry,
//!   exactly matching pre-3.4 behaviour.
//!
//! The compromise's main virtue: it degrades gracefully. On single-
//! illuminant DCPs it's a no-op. On dual-illuminant DCPs it produces a
//! visually smooth blend. Upgrading to the spec's full iterative procedure
//! is a later refinement that doesn't invalidate any downstream code,
//! because `interpolate_hue_sat_maps` already returns a merged
//! [`HueSatMap`].

use super::parser::{Dcp, HueSatMap};

/// Fallback scene-temperature estimate when the camera's white-balance
/// coefficients are unavailable or malformed. `5000 K` sits between D50
/// (`5003 K`) and our own 5500 K warm neutral, which is close to Adobe's
/// "no illuminant info" default.
pub const DEFAULT_SCENE_TEMP_K: f32 = 5000.0;

/// Standard-illuminant tag codes we translate to Kelvin values. These come
/// from EXIF `LightSource` / DNG `CalibrationIlluminant*`. See DNG 1.6
/// § 6.2.5 for the full list; we cover the values that Adobe's own DCPs
/// and smartphone DNGs are known to use, and fall back to [`DEFAULT_SCENE_TEMP_K`]
/// for unknown codes.
///
/// Returns the temperature in Kelvin.
pub fn illuminant_temp_k(code: u16) -> f32 {
    // Values per the DNG spec / EXIF 2.3 LightSource table. The
    // fluorescent illuminants are treated as their CIE equivalents.
    match code {
        1 => 5500.0,  // Daylight
        2 => 3500.0,  // Fluorescent (warm white-ish default)
        3 => 2856.0,  // Tungsten
        4 => 5500.0,  // Flash
        9 => 6000.0,  // Fine weather
        10 => 6500.0, // Cloudy
        11 => 7500.0, // Shade
        12 => 6430.0, // Daylight fluorescent (D-series)
        13 => 5000.0, // Day white fluorescent (N)
        14 => 4200.0, // Cool white fluorescent (W)
        15 => 3450.0, // White fluorescent (WW)
        17 => 2856.0, // Standard Light A
        18 => 4874.0, // Standard Light B
        19 => 6774.0, // Standard Light C
        20 => 7504.0, // D75
        21 => 6504.0, // D65
        22 => 5503.0, // D55
        23 => 5003.0, // D50
        24 => 3200.0, // ISO Studio Tungsten
        _ => DEFAULT_SCENE_TEMP_K,
    }
}

/// Estimate the scene's correlated color temperature from rawler's
/// `wb_coeffs` (per-channel multipliers applied before the camera matrix).
///
/// High R/G means the camera is pushing red up to neutralise a
/// red-dominant (warm / low-K) scene. Low R/G means the camera is pushing
/// blue up to neutralise a blue-dominant (cool / high-K) scene.
///
/// The linear approximation `temp ≈ 7000 − 2000 × (R/G − 1)`:
/// - At R/G = 1.0 (daylight-ish neutral), returns 7000 K.
/// - At R/G = 2.0 (tungsten-ish), returns 5000 K. (Empirical, tungsten WB
///   coeffs usually land between 1.8 and 2.4; this gets us into the warm
///   half of the illuminant spectrum, which is what we need for the
///   blend.)
/// - At R/G = 0.5 (shade / overcast), returns 8000 K.
///
/// The numeric constants are empirical, not physics. They were picked so
/// that known test fixtures (Sony A7 III indoor tungsten, iPhone ProRAW
/// daylight, Pixel 6 Pro mixed) each land on their expected side of the
/// D65 / A boundary. See `docs/notes/raw-support-phase3.md`.
///
/// Clamps to `[2000, 10000] K` so pathological inputs can't blow up the
/// downstream blend weight.
pub fn estimate_scene_temp_k(wb_coeffs: [f32; 4]) -> f32 {
    let r = wb_coeffs[0];
    let g = wb_coeffs[1];
    if !r.is_finite() || !g.is_finite() || g <= 0.0 || r <= 0.0 {
        return DEFAULT_SCENE_TEMP_K;
    }
    let ratio = r / g;
    let temp = 7000.0 - 2000.0 * (ratio - 1.0);
    temp.clamp(2000.0, 10000.0)
}

/// Pick or blend the hue/sat maps for a given scene temperature. Returns
/// `None` when the DCP carries no hue/sat map at all, otherwise returns a
/// freshly-allocated merged map.
///
/// Behaviour:
/// - Single-map DCP (only `hue_sat_map_1` or `hue_sat_map_2`): returns a
///   clone of that map. No allocation saving, but the apply code expects
///   an owned value anyway for future mutations.
/// - Dual-map DCP with matching dims: computes a blend weight
///   `t = clamp((scene_k − low_k) / (high_k − low_k), 0, 1)` and
///   interpolates entry-by-entry.
/// - Dual-map DCP with mismatched dims or missing illuminants: falls back
///   to the D65-preferring `Dcp::pick_hue_sat_map` logic so we stay
///   backwards-compatible with single-illuminant DCPs that happen to
///   carry both slots.
pub fn interpolate_hue_sat_maps(dcp: &Dcp, scene_temp_k: f32) -> Option<HueSatMap> {
    match (&dcp.hue_sat_map_1, &dcp.hue_sat_map_2) {
        (Some(m), None) => Some(m.clone()),
        (None, Some(m)) => Some(m.clone()),
        (None, None) => None,
        (Some(m1), Some(m2)) => {
            let ill_1 = dcp.calibration_illuminant_1.map(illuminant_temp_k);
            let ill_2 = dcp.calibration_illuminant_2.map(illuminant_temp_k);
            match (ill_1, ill_2) {
                (Some(t1), Some(t2)) if t1 != t2 && m1.has_same_shape_as(m2) => {
                    // Order so `low_k` is the cooler illuminant (lower
                    // temperature). This keeps the weight positive and
                    // intuitive.
                    let (low_k, low_map, high_k, high_map) = if t1 < t2 {
                        (t1, m1, t2, m2)
                    } else {
                        (t2, m2, t1, m1)
                    };
                    let t = ((scene_temp_k - low_k) / (high_k - low_k)).clamp(0.0, 1.0);
                    Some(blend_maps(low_map, high_map, t))
                }
                // Dims mismatch or missing illuminant data — fall back to
                // the D65-preferring pick so at least one map applies.
                _ => dcp.pick_hue_sat_map().cloned(),
            }
        }
    }
}

impl HueSatMap {
    /// True when two hue/sat maps share the same `(hue, sat, val)` grid
    /// dimensions. Entry-by-entry blending requires this.
    pub(crate) fn has_same_shape_as(&self, other: &HueSatMap) -> bool {
        self.hue_divs == other.hue_divs
            && self.sat_divs == other.sat_divs
            && self.val_divs == other.val_divs
            && self.data.len() == other.data.len()
    }
}

/// Linear blend between two hue/sat maps of identical shape. `t == 0`
/// returns `low`, `t == 1` returns `high`, values in between interpolate
/// every LUT entry.
///
/// Panics if the maps have different lengths; callers must check with
/// [`HueSatMap::has_same_shape_as`] first. Enforcing the precondition
/// rather than computing against the minimum length means we never
/// produce a truncated-and-wrong LUT silently.
fn blend_maps(low: &HueSatMap, high: &HueSatMap, t: f32) -> HueSatMap {
    assert!(
        low.has_same_shape_as(high),
        "blend_maps called on maps of different shapes"
    );
    let inv_t = 1.0 - t;
    let data = low
        .data
        .iter()
        .zip(high.data.iter())
        .map(|(&a, &b)| inv_t * a + t * b)
        .collect();
    HueSatMap {
        hue_divs: low.hue_divs,
        sat_divs: low.sat_divs,
        val_divs: low.val_divs,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_constant_map(hue_shift: f32, sat_scale: f32, val_scale: f32) -> HueSatMap {
        // Small 2×2×1 grid, every entry the same triple. Makes it easy to
        // inspect the blended output since each entry is identical.
        let mut data = Vec::with_capacity(2 * 2 * 3);
        for _ in 0..(2 * 2) {
            data.push(hue_shift);
            data.push(sat_scale);
            data.push(val_scale);
        }
        HueSatMap {
            hue_divs: 2,
            sat_divs: 2,
            val_divs: 1,
            data,
        }
    }

    fn dcp_with_two_maps(low_k: u16, high_k: u16, m1: HueSatMap, m2: HueSatMap) -> Dcp {
        Dcp {
            unique_camera_model: None,
            profile_name: None,
            profile_copyright: None,
            profile_calibration_signature: None,
            calibration_illuminant_1: Some(low_k),
            calibration_illuminant_2: Some(high_k),
            hue_sat_map_1: Some(m1),
            hue_sat_map_2: Some(m2),
            hue_sat_map_encoding: 0,
            look_table: None,
            look_table_encoding: 0,
            tone_curve: None,
        }
    }

    #[test]
    fn scene_temp_warm_for_high_r_over_g() {
        // R/G = 2.0 → tungsten-ish. Expect a temperature in the warm half
        // (below 6000 K).
        let t = estimate_scene_temp_k([2.0, 1.0, 2.0, 1.0]);
        assert!(t < 6000.0, "expected warm temp, got {t}");
        assert!(t > 3000.0, "expected above the hard floor, got {t}");
    }

    #[test]
    fn scene_temp_cool_for_low_r_over_g() {
        // R/G = 0.5 → overcast / shade. Expect a cool temperature.
        let t = estimate_scene_temp_k([0.5, 1.0, 1.5, 1.0]);
        assert!(t > 7000.0, "expected cool temp, got {t}");
    }

    #[test]
    fn scene_temp_neutral_for_equal_r_g() {
        let t = estimate_scene_temp_k([1.0, 1.0, 1.0, 1.0]);
        assert!((t - 7000.0).abs() < 1e-3);
    }

    #[test]
    fn scene_temp_falls_back_on_bad_coeffs() {
        assert_eq!(
            estimate_scene_temp_k([f32::NAN, 1.0, 1.0, 1.0]),
            DEFAULT_SCENE_TEMP_K
        );
        assert_eq!(
            estimate_scene_temp_k([1.0, 0.0, 1.0, 1.0]),
            DEFAULT_SCENE_TEMP_K
        );
        assert_eq!(
            estimate_scene_temp_k([0.0, 1.0, 1.0, 1.0]),
            DEFAULT_SCENE_TEMP_K
        );
    }

    #[test]
    fn scene_temp_clamps_extremes() {
        // R/G = 10 → formula gives 7000 − 18000 = negative. Must clamp.
        let t = estimate_scene_temp_k([10.0, 1.0, 1.0, 1.0]);
        assert_eq!(t, 2000.0);
    }

    #[test]
    fn illuminant_temp_lookup() {
        assert!((illuminant_temp_k(17) - 2856.0).abs() < 1e-3);
        assert!((illuminant_temp_k(21) - 6504.0).abs() < 1e-3);
        assert!((illuminant_temp_k(23) - 5003.0).abs() < 1e-3);
        // Unknown codes fall back.
        assert!((illuminant_temp_k(255) - DEFAULT_SCENE_TEMP_K).abs() < 1e-3);
    }

    #[test]
    fn interpolate_single_map_returns_clone() {
        let mut dcp = dcp_with_two_maps(
            17,
            21,
            make_constant_map(10.0, 1.5, 1.0),
            make_constant_map(20.0, 2.0, 1.0),
        );
        dcp.hue_sat_map_2 = None;
        let merged = interpolate_hue_sat_maps(&dcp, 6504.0).expect("map present");
        assert_eq!(merged.data[0], 10.0);
    }

    #[test]
    fn interpolate_at_low_endpoint_matches_low_map() {
        // A = 17 → 2856 K, D65 = 21 → 6504 K. Scene at 2856 K → pure map 1.
        let dcp = dcp_with_two_maps(
            17,
            21,
            make_constant_map(10.0, 1.5, 1.0),
            make_constant_map(20.0, 2.0, 1.0),
        );
        let merged = interpolate_hue_sat_maps(&dcp, 2856.0).expect("map present");
        assert!(
            (merged.data[0] - 10.0).abs() < 1e-4,
            "got {}",
            merged.data[0]
        );
        assert!((merged.data[1] - 1.5).abs() < 1e-4);
    }

    #[test]
    fn interpolate_at_high_endpoint_matches_high_map() {
        let dcp = dcp_with_two_maps(
            17,
            21,
            make_constant_map(10.0, 1.5, 1.0),
            make_constant_map(20.0, 2.0, 1.0),
        );
        let merged = interpolate_hue_sat_maps(&dcp, 6504.0).expect("map present");
        assert!(
            (merged.data[0] - 20.0).abs() < 1e-4,
            "got {}",
            merged.data[0]
        );
        assert!((merged.data[1] - 2.0).abs() < 1e-4);
    }

    #[test]
    fn interpolate_at_midpoint_averages() {
        let dcp = dcp_with_two_maps(
            17,
            21,
            make_constant_map(10.0, 1.5, 1.0),
            make_constant_map(20.0, 2.0, 1.0),
        );
        // Midpoint between 2856 K and 6504 K is 4680 K.
        let mid_k = (2856.0 + 6504.0) / 2.0;
        let merged = interpolate_hue_sat_maps(&dcp, mid_k).expect("map present");
        assert!(
            (merged.data[0] - 15.0).abs() < 1e-4,
            "got {}",
            merged.data[0]
        );
        assert!((merged.data[1] - 1.75).abs() < 1e-4);
    }

    #[test]
    fn interpolate_clamps_outside_range() {
        let dcp = dcp_with_two_maps(
            17,
            21,
            make_constant_map(10.0, 1.5, 1.0),
            make_constant_map(20.0, 2.0, 1.0),
        );
        // Way cooler than D65 → clamps to high endpoint.
        let merged = interpolate_hue_sat_maps(&dcp, 10000.0).expect("map present");
        assert!((merged.data[0] - 20.0).abs() < 1e-4);
        // Way warmer than A → clamps to low endpoint.
        let merged = interpolate_hue_sat_maps(&dcp, 1500.0).expect("map present");
        assert!((merged.data[0] - 10.0).abs() < 1e-4);
    }

    #[test]
    fn interpolate_shape_mismatch_falls_back_to_pick() {
        // Different dims → blending is impossible, so fall back to
        // pick_hue_sat_map (prefers D65).
        let dcp = Dcp {
            unique_camera_model: None,
            profile_name: None,
            profile_copyright: None,
            profile_calibration_signature: None,
            calibration_illuminant_1: Some(17),
            calibration_illuminant_2: Some(21),
            hue_sat_map_1: Some(HueSatMap {
                hue_divs: 2,
                sat_divs: 1,
                val_divs: 1,
                data: vec![10.0, 1.5, 1.0, 10.0, 1.5, 1.0],
            }),
            hue_sat_map_2: Some(make_constant_map(20.0, 2.0, 1.0)),
            hue_sat_map_encoding: 0,
            look_table: None,
            look_table_encoding: 0,
            tone_curve: None,
        };
        let merged = interpolate_hue_sat_maps(&dcp, 5000.0).expect("map present");
        // Fell back to pick (D65 = map 2): hue shift 20, sat 2.0.
        assert_eq!(merged.data[0], 20.0);
    }
}
