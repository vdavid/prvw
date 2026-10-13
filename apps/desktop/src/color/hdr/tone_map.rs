//! Mode 3's tone map: scene-linear HDR (PQ and HLG PNGs, float TIFFs) brought down to whatever the
//! display can show right now, and the content-peak estimate it needs.
//!
//! - [`eetf`] is BT.2390's EETF, the Hermite-spline knee BT.2408 Annex 5 uses, applied to
//!   luminance in the PQ domain with black lift off. It maps `content_peak` exactly onto the
//!   target, leaves everything below the knee alone, and is the identity when the content fits.
//! - [`tone_map`] runs it on a pixel's luminance and scales R, G, and B by the same factor, which
//!   keeps the hue, the way the RAW tone curve does.
//! - [`PeakHistogram`] estimates `content_peak` as the 99.9th percentile of luminance, so a few
//!   stray hot pixels don't squash the whole image.
//!
//! `render/shader.wgsl` ports [`eetf`] and [`tone_map`] for the GPU, at the live headroom; the CPU
//! runs them for real with a target of 1.0, the SDR fallback for displays that can't show HDR.

use super::gain_map::Mat3;
use super::transfer::{REFERENCE_WHITE_NITS, pq_eotf, pq_inverse_eotf};

/// What mode 3 needs to know about one scene-linear image (`PixelEncoding::SceneLinear`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneLinearParams {
    /// Linear source primaries into `HdrDisplaySpace::for_host()`, white to white.
    pub source_to_display: Mat3,
    /// The image's brightest meaningful luminance, relative to SDR white
    /// ([`PeakHistogram::peak`]).
    pub content_peak: f32,
    /// The source primaries' luminance coefficients (the Y row of their RGB to XYZ matrix).
    pub luminance_weights: [f32; 3],
}

/// BT.2390's EETF on a luminance `y` relative to SDR white: content up to `content_peak` mapped
/// onto a display that reaches `target_peak`.
pub fn eetf(y: f32, content_peak: f32, target_peak: f32) -> f32 {
    match Knee::new(content_peak, target_peak) {
        Some(knee) if y > 0.0 => knee.apply(y),
        _ => y,
    }
}

/// [`eetf`] on `rgb`'s luminance, with R, G, and B scaled together so the hue stays.
pub fn tone_map(rgb: [f32; 3], weights: [f32; 3], content_peak: f32, target_peak: f32) -> [f32; 3] {
    let y = weights[0] * rgb[0] + weights[1] * rgb[1] + weights[2] * rgb[2];
    if y <= 0.0 {
        return rgb;
    }
    let ratio = eetf(y, content_peak, target_peak) / y;
    rgb.map(|v| v * ratio)
}

/// The EETF's constants for one source and target, in BT.2390's normalized PQ domain, where the
/// content peak is 1.0. `None` when the content fits, since the curve is then the identity.
#[derive(Clone, Copy, Debug)]
struct Knee {
    /// The content peak as a PQ signal: what normalizes the domain.
    source_pq: f32,
    /// The target peak in the normalized domain (`maxLum`).
    max_lum: f32,
    /// Where the curve leaves the identity (`KS`).
    start: f32,
}

impl Knee {
    fn new(content_peak: f32, target_peak: f32) -> Option<Self> {
        if content_peak.is_nan() || content_peak <= target_peak {
            return None;
        }
        let source_pq = pq_inverse_eotf(content_peak * REFERENCE_WHITE_NITS);
        let max_lum = pq_inverse_eotf(target_peak * REFERENCE_WHITE_NITS) / source_pq;
        Some(Self {
            source_pq,
            max_lum,
            // Never below 0: a target far below the content would put the knee there, and the
            // spline needs `start <= e <= 1`.
            start: (1.5 * max_lum - 0.5).max(0.0),
        })
    }

    fn apply(self, y: f32) -> f32 {
        // The 0.1% of pixels above the content peak land on the target with it.
        let e1 = (pq_inverse_eotf(y * REFERENCE_WHITE_NITS) / self.source_pq).min(1.0);
        let e2 = if e1 < self.start {
            e1
        } else {
            let t = (e1 - self.start) / (1.0 - self.start);
            let (t2, t3) = (t * t, t * t * t);
            // The spline's slope is 1.5 × (1 − max_lum) × (t − 1)², so it never overshoots while
            // `start` is unclamped; the `min` covers a target so far below the content that it
            // is (a source peak past about 10⁸ nits).
            ((2.0 * t3 - 3.0 * t2 + 1.0) * self.start
                + (t3 - 2.0 * t2 + t) * (1.0 - self.start)
                + (-2.0 * t3 + 3.0 * t2) * self.max_lum)
                .min(self.max_lum)
        };
        pq_eotf(e2 * self.source_pq) / REFERENCE_WHITE_NITS
    }
}

/// Which share of pixels the content peak sits above: the 99.9th percentile of luminance.
const PEAK_PERCENTILE: f64 = 0.999;

/// [`PeakHistogram`]'s range, as f32 exponents: luminance from 2⁻¹⁶ to 2¹⁶ of SDR white. Below
/// counts as black; above is brighter than anything a display will show, and lands in the top
/// bin.
const MIN_EXPONENT: i32 = -16;
const MAX_EXPONENT: i32 = 16;
/// Bins per octave: the top five mantissa bits.
const BINS_PER_OCTAVE_BITS: u32 = 5;
pub const PEAK_BINS: usize = ((MAX_EXPONENT - MIN_EXPONENT) as usize) << BINS_PER_OCTAVE_BITS;
/// An f32's bit pattern shifted right by this keeps its exponent and the top mantissa bits.
const BIN_SHIFT: u32 = 23 - BINS_PER_OCTAVE_BITS;
const FIRST_BIN: u32 = ((MIN_EXPONENT + 127) as u32) << BINS_PER_OCTAVE_BITS;

/// A log2 luminance histogram, 1,024 bins of 1/32 stop each, binned straight off the f32 bit
/// pattern (exponent plus the top five mantissa bits), so a pixel costs a shift and no `log2`.
/// Each decode thread fills its own and they [`merge`](Self::merge).
#[derive(Clone, Debug)]
pub struct PeakHistogram {
    counts: Vec<u32>,
    /// The brightest luminance in each bin, so the peak is a value some pixel has: a flat image
    /// peaks at exactly its level, not at its bin's upper edge, which would tip an SDR float
    /// TIFF at 1.0 over into HDR.
    maxima: Vec<f32>,
    total: u64,
}

impl Default for PeakHistogram {
    fn default() -> Self {
        Self {
            counts: vec![0; PEAK_BINS],
            maxima: vec![0.0; PEAK_BINS],
            total: 0,
        }
    }
}

impl PeakHistogram {
    pub fn add(&mut self, y: f32) {
        self.total += 1;
        if y.is_nan() || y <= 0.0 {
            self.counts[0] += 1;
            return;
        }
        let bin =
            ((y.to_bits() >> BIN_SHIFT).saturating_sub(FIRST_BIN) as usize).min(PEAK_BINS - 1);
        self.counts[bin] += 1;
        self.maxima[bin] = self.maxima[bin].max(y);
    }

    pub fn merge(mut self, other: Self) -> Self {
        for (a, b) in self.counts.iter_mut().zip(&other.counts) {
            *a += b;
        }
        for (a, b) in self.maxima.iter_mut().zip(&other.maxima) {
            *a = a.max(*b);
        }
        self.total += other.total;
        self
    }

    /// The 99.9th-percentile luminance, to within a bin (1/32 stop): the brightest pixel in the
    /// bin the percentile falls in. 0 for an empty or all-black histogram.
    pub fn peak(&self) -> f32 {
        let wanted = (self.total as f64 * PEAK_PERCENTILE).ceil() as u64;
        let mut seen = 0u64;
        for (i, &n) in self.counts.iter().enumerate() {
            seen += u64::from(n);
            if seen >= wanted && n > 0 {
                return self.maxima[i];
            }
        }
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1,000-nit content, the common PQ master, against a few display headrooms.
    const CONTENT: f32 = 1000.0 / 203.0;
    const TARGETS: [f32; 3] = [1.0, 2.0, 4.0];

    fn close(got: f32, want: f32, tolerance: f32) -> bool {
        (got - want).abs() <= tolerance * want.abs().max(1e-6)
    }

    /// Where the curve starts bending, as a luminance.
    fn knee_luminance(content_peak: f32, target: f32) -> f32 {
        let k = Knee::new(content_peak, target).expect("the content doesn't fit");
        pq_eotf(k.start * k.source_pq) / REFERENCE_WHITE_NITS
    }

    #[test]
    fn the_eetf_is_the_identity_below_the_knee() {
        for target in TARGETS {
            let knee = knee_luminance(CONTENT, target);
            assert!(knee > 0.1 && knee < target, "knee at {knee} for {target}");
            for i in 1..=100 {
                let y = knee * i as f32 / 101.0;
                assert!(close(eetf(y, CONTENT, target), y, 1e-4), "{y} at {target}");
            }
        }
    }

    #[test]
    fn the_eetf_maps_the_content_peak_onto_the_target() {
        for target in TARGETS {
            let got = eetf(CONTENT, CONTENT, target);
            assert!(close(got, target, 1e-4), "{got} for {target}");
        }
        // Far brighter content too: a float TIFF can pass PQ's 10,000 nits.
        let got = eetf(500.0, 500.0, 1.0);
        assert!(close(got, 1.0, 1e-4), "{got}");
    }

    #[test]
    fn the_eetf_is_monotonic_and_stays_under_the_target() {
        for target in TARGETS {
            let mut last = 0.0;
            for i in 0..=4000 {
                let y = CONTENT * 1.5 * i as f32 / 4000.0;
                let got = eetf(y, CONTENT, target);
                assert!(got >= last, "falls at {y} for {target}: {got} < {last}");
                // 1e-4: the PQ round trip in f32, not the curve, which provably never overshoots.
                assert!(got <= target * (1.0 + 1e-4), "{got} over {target} at {y}");
                last = got;
            }
        }
    }

    #[test]
    fn the_eetf_is_continuous_at_the_knee() {
        for target in TARGETS {
            let knee = knee_luminance(CONTENT, target);
            let below = eetf(knee * (1.0 - 1e-4), CONTENT, target);
            let above = eetf(knee * (1.0 + 1e-4), CONTENT, target);
            assert!(close(above, below, 1e-3), "{below} vs {above} at {target}");
        }
    }

    #[test]
    fn the_eetf_is_the_identity_when_the_content_fits() {
        for y in [0.0, 0.01, 0.5, 1.0, 2.0, 3.9, 6.0] {
            assert_eq!(eetf(y, 4.0, 4.0), y);
            assert_eq!(eetf(y, 2.0, 8.0), y);
        }
    }

    /// The SDR fallback's case: 1,000-nit content onto SDR white. Reference white comes down a
    /// little to make room, shadows don't move: a photo, not grey.
    #[test]
    fn reference_white_stays_bright_on_an_sdr_target() {
        let white = eetf(1.0, CONTENT, 1.0);
        assert!(white > 0.7 && white < 1.0, "{white}");
        assert!(close(eetf(0.1, CONTENT, 1.0), 0.1, 1e-4));
    }

    #[test]
    fn tone_map_keeps_the_hue_and_maps_luminance() {
        let weights = [0.2627, 0.6780, 0.0593];
        let rgb = [6.0, 3.0, 1.5];
        let out = tone_map(rgb, weights, 8.0, 2.0);
        let y = |c: [f32; 3]| weights[0] * c[0] + weights[1] * c[1] + weights[2] * c[2];
        assert!(close(y(out), eetf(y(rgb), 8.0, 2.0), 1e-5));
        assert!(close(out[0] / out[1], 2.0, 1e-5) && close(out[1] / out[2], 2.0, 1e-5));
        // Black and below stay put.
        assert_eq!(tone_map([0.0; 3], weights, 8.0, 2.0), [0.0; 3]);
        assert_eq!(
            tone_map([-0.1, 0.0, 0.0], weights, 8.0, 2.0),
            [-0.1, 0.0, 0.0]
        );
    }

    fn histogram(values: impl IntoIterator<Item = f32>) -> PeakHistogram {
        let mut h = PeakHistogram::default();
        for y in values {
            h.add(y);
        }
        h
    }

    #[test]
    fn a_flat_image_peaks_at_its_level() {
        for level in [0.25, 1.0, 4.0, 37.5] {
            let peak = histogram(std::iter::repeat_n(level, 500)).peak();
            assert_eq!(peak, level);
        }
    }

    /// One pixel in a thousand above the rest is the 0.1% the percentile ignores; one in a
    /// hundred is a highlight, and sets the peak to within a bin.
    #[test]
    fn the_peak_ignores_stray_hot_pixels_but_not_highlights() {
        let stray = histogram((0..1000).map(|i| if i == 0 { 100.0 } else { 1.0 }));
        assert_eq!(stray.peak(), 1.0);
        let highlight = histogram((0..1000).map(|i| if i < 10 { 100.0 } else { 1.0 }));
        assert!(
            close(highlight.peak(), 100.0, 0.035),
            "{}",
            highlight.peak()
        );
    }

    #[test]
    fn the_peak_tracks_a_ramp_to_within_a_bin() {
        // 0.0 to 8.0 in even steps: the 99.9th percentile is 7.992.
        let ramp = histogram((0..10_000).map(|i| 8.0 * i as f32 / 9_999.0));
        assert!(close(ramp.peak(), 7.992, 0.035), "{}", ramp.peak());
    }

    #[test]
    fn black_nan_and_out_of_range_values_never_panic() {
        let h = histogram([0.0, -1.0, f32::NAN, f32::INFINITY, 1e-30, 1e30]);
        assert!(!h.peak().is_nan());
        assert_eq!(PeakHistogram::default().peak(), 0.0);
        assert_eq!(histogram([0.0; 10]).peak(), 0.0);
    }

    #[test]
    fn merged_histograms_agree_with_one() {
        let values: Vec<f32> = (0..5000)
            .map(|i| (i as f32 * 0.37).sin().abs() * 12.0)
            .collect();
        let whole = histogram(values.iter().copied());
        let (a, b) = values.split_at(1234);
        let merged = histogram(a.iter().copied()).merge(histogram(b.iter().copied()));
        assert_eq!(merged.peak(), whole.peak());
    }
}
