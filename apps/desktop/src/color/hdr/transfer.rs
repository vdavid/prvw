//! The BT.2100 transfer functions that turn PQ and HLG signals into light, in the app's terms:
//! linear, relative to SDR white, with 1.0 = 203 nits (BT.2408's reference white).
//!
//! `render/shader.wgsl` ports [`pq_eotf`] and [`pq_inverse_eotf`] for mode 3's tone map, which
//! runs in the PQ domain; the HLG functions only run on the CPU, at decode.

/// SDR white in nits: BT.2408's HDR reference white, which is what 1.0 means everywhere in the
/// HDR pipeline.
pub const REFERENCE_WHITE_NITS: f32 = 203.0;

/// The nominal peak an HLG signal is shown at. BT.2100's OOTF needs one, and 1,000 nits is the
/// reference display the system gamma of 1.2 belongs to.
pub const HLG_NOMINAL_PEAK_NITS: f32 = 1000.0;

// SMPTE ST 2084's constants, exactly as the standard writes them.
const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f32 = 2392.0 / 4096.0 * 32.0;
const PQ_PEAK_NITS: f32 = 10_000.0;

/// SMPTE ST 2084's EOTF: a PQ signal (0 to 1) to absolute display light in nits.
pub fn pq_eotf(signal: f32) -> f32 {
    let p = signal.max(0.0).powf(1.0 / PQ_M2);
    let ratio = (p - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * p);
    PQ_PEAK_NITS * ratio.powf(1.0 / PQ_M1)
}

/// The inverse: absolute light in nits to a PQ signal. Above 10,000 nits it keeps climbing past
/// 1.0, toward 2.0, which the tone map needs for float sources brighter than PQ can carry.
pub fn pq_inverse_eotf(nits: f32) -> f32 {
    let y = (nits.max(0.0) / PQ_PEAK_NITS).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M2)
}

// BT.2100's HLG constants.
const HLG_A: f32 = 0.178_832_77;
const HLG_B: f32 = 1.0 - 4.0 * HLG_A;
// 0.5 - a × ln(4a), spelled out because `ln` isn't const.
const HLG_C: f32 = 0.559_910_7;
const HLG_SYSTEM_GAMMA: f32 = 1.2;

/// BT.2100's inverse OETF: an HLG signal (0 to 1) to normalized scene light (0 to 1).
pub fn hlg_inverse_oetf(signal: f32) -> f32 {
    let e = signal.clamp(0.0, 1.0);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

/// BT.2100's HLG OOTF for the nominal 1,000-nit display (system gamma 1.2, no black lift): scene
/// light to display light, relative to SDR white. `weights` are the signal's primaries'
/// luminance coefficients, which give the OOTF its scene luminance.
pub fn hlg_ootf(scene: [f32; 3], weights: [f32; 3]) -> [f32; 3] {
    let y = (weights[0] * scene[0] + weights[1] * scene[1] + weights[2] * scene[2]).max(0.0);
    let gain = HLG_NOMINAL_PEAK_NITS / REFERENCE_WHITE_NITS * y.powf(HLG_SYSTEM_GAMMA - 1.0);
    scene.map(|v| v * gain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(got: f32, want: f32, tolerance: f32) -> bool {
        (got - want).abs() <= tolerance * want.abs().max(1e-6)
    }

    /// The spec's reference values (section 5), which are BT.2408's.
    #[test]
    fn pq_eotf_hits_the_reference_values() {
        assert!(close(pq_eotf(0.5806), 203.0, 0.005), "{}", pq_eotf(0.5806));
        assert!(close(pq_eotf(0.7518), 1000.0, 0.005), "{}", pq_eotf(0.7518));
        assert!(close(pq_eotf(1.0), 10_000.0, 1e-4), "{}", pq_eotf(1.0));
        assert_eq!(pq_eotf(0.0), 0.0);
    }

    #[test]
    fn pq_inverse_eotf_undoes_the_eotf() {
        for signal in [0.01, 0.1, 0.3, 0.5806, 0.7518, 0.9, 1.0] {
            let back = pq_inverse_eotf(pq_eotf(signal));
            assert!(close(back, signal, 1e-4), "{signal}: {back}");
        }
        assert!(close(pq_inverse_eotf(203.0), 0.5806, 0.001));
    }

    /// Past 10,000 nits the inverse keeps rising (a float TIFF can be brighter than PQ goes),
    /// so the tone map's PQ-domain math stays monotonic there.
    #[test]
    fn pq_inverse_eotf_keeps_climbing_above_10000_nits() {
        let mut last = pq_inverse_eotf(10_000.0);
        for nits in [20_000.0, 100_000.0, 1e6, 1e8] {
            let next = pq_inverse_eotf(nits);
            assert!(next > last && next < 2.01, "{nits}: {next}");
            last = next;
        }
    }

    /// HLG's 75% signal is reference white: grey there lands at 1.0, within BT.2408's rounding.
    #[test]
    fn hlg_reference_white_lands_at_sdr_white() {
        let bt2020 = [0.2627, 0.6780, 0.0593];
        let scene = hlg_inverse_oetf(0.75);
        let display = hlg_ootf([scene; 3], bt2020);
        assert!(close(display[0], 1.0, 0.01), "{display:?}");
        // And the full signal is the nominal peak.
        let peak = hlg_ootf([hlg_inverse_oetf(1.0); 3], bt2020);
        assert!(close(peak[1], 1000.0 / 203.0, 1e-3), "{peak:?}");
    }

    /// The two halves of the inverse OETF meet at a signal of 0.5 (scene light 1/12).
    #[test]
    fn hlg_inverse_oetf_is_continuous_at_its_knee() {
        assert!(close(hlg_inverse_oetf(0.5), 1.0 / 12.0, 1e-6));
        assert!(close(hlg_inverse_oetf(0.500_001), 1.0 / 12.0, 1e-4));
        assert_eq!(hlg_inverse_oetf(0.0), 0.0);
        assert!(close(hlg_inverse_oetf(1.0), 1.0, 1e-5));
    }
}
