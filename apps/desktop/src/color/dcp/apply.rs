//! Apply a DCP `HueSatMap` to a linear Rec.2020 buffer.
//!
//! The DNG spec defines the hue/sat map as a 3D LUT indexed by a pixel's
//! **normalized HSV** coordinates. For each pixel:
//!
//! 1. Convert (R, G, B) to (H, S, V).
//!    - H in `[0, 6)` (six hue sectors, matching the spec's "hue 0..1" after
//!      multiplying by 6; we use the 0..6 parametrization internally).
//!    - S in `[0, 1]`.
//!    - V = max(R, G, B), unbounded above.
//! 2. Trilinearly interpolate the LUT at `(H, S, V)` to get
//!    `(hue_shift_degrees, sat_scale, val_scale)`.
//! 3. Apply: `H' = H + hue_shift / 60`, `S' = clamp(S * sat_scale, 0, 1)`,
//!    `V' = V * val_scale`.
//! 4. Convert back to (R, G, B) in the same RGB space.
//!
//! ## HSV conventions
//!
//! The DNG spec's "HSV" is computed straight on the linear-light RGB values
//! we pass in — there's no gamma bake-in. Hue wraps modulo 6 (or 360°).
//! Saturation and value use the standard `max(R,G,B)` / `max(R,G,B)-min(R,G,B)`
//! definitions. We follow the spec exactly.
//!
//! ## Hue wraparound
//!
//! The hue axis is cyclic: index `hue_divs` maps back to index 0. So a pixel
//! at `H = 5.9` with a 90-entry LUT samples between index 88 and 89, but a
//! pixel at `H = 5.99` (close to wrapping) samples between index 89 and
//! index 0 — not index 89 and 90. We handle this by reading the "next" index
//! modulo `hue_divs`.
//!
//! ## Saturation and value axes
//!
//! Unlike hue, saturation and value do **not** wrap. Input values are
//! clamped to `[0, sat_divs-1]` and `[0, val_divs-1]` before interpolating.
//! When `val_divs == 1`, the value axis is entirely absent and all pixels
//! share the same "row" — common in Adobe's 2D profiles.
//!
//! ## Value-axis encoding
//!
//! `ProfileHueSatMapEncoding == 0` means the value axis is linear in
//! `V = max(R, G, B)`. `== 1` means it's linear in sRGB-gamma'd
//! `V' = sRGB_encode(V)`. Most user-facing DCPs use encoding 0; we support
//! both, but warn once if we see 1 so we can revisit if the output looks
//! wrong. (Adobe's ACR applies the encoding to match its internal sRGB-
//! gamma working space; our working space is linear Rec.2020, so encoding 0
//! is the correct assumption most of the time.)
//!
//! ## Safety
//!
//! - Empty buffers and single-entry LUTs both no-op.
//! - A LUT with all-zero hue shifts and all-ones sat/val scales passes
//!   every pixel through unchanged (enforced by a unit test).
//! - Pixels with all channels equal (neutral gray) have `S = 0`, which
//!   means hue is ill-defined; we short-circuit and only apply `val_scale`
//!   so neutrals never drift chroma.
//!
//! ## SIMD vectorization (Phase 6.5)
//!
//! The per-chunk inner loop `apply_pixels_chunk` is annotated with
//! `#[multiversion]` — NEON on aarch64, AVX2+FMA on x86_64. The compiler
//! emits one copy of the function per target and a dispatch stub picks the
//! best at runtime.
//!
//! The trilinear LUT lerps use `f32::mul_add` so FMA emits on both ISAs
//! (two FMAs per lerp × 7 lerps × 3 channels = 42 FMAs per pixel — the
//! fraction of the per-pixel cost SIMD actually helps with). `rem_euclid`
//! calls in the hue-wrap and sector-index code are replaced with
//! conditional adjusts (`wrap_hue_6`, `hue_index_wrap`) — `rem_euclid` on
//! f32 calls out to `fmodf`, which is dramatically slower than a compare-
//! and-add on Apple Silicon.
//!
//! The 8-corner LUT gather in `sample_lut` is intentionally left scalar.
//! NEON / AVX2 gather intrinsics are emulated as scalar loads on
//! Apple Silicon and are routinely slower than the compiler's straight
//! scalar sequence when cache behavior is already good (for a 90×30×1
//! HSM the whole LUT fits in L1). The 7 trilinear lerps that follow the
//! gather are the vectorizable math we care about.

use multiversion::multiversion;
use rayon::prelude::*;

use super::parser::HueSatMap;

/// Pixels per parallel work unit. Big enough to amortise rayon overhead,
/// small enough that each chunk fits comfortably in L1 even for a 90×30×1
/// LUT worth of corner fetches.
const PIXELS_PER_CHUNK: usize = 1024;

/// Apply the hue/sat map to a flat linear-Rec.2020 buffer in place. Layout
/// is `[R0, G0, B0, R1, G1, B1, …]`; length must be a multiple of 3.
///
/// `value_encoding`:
/// - `0` = linear value axis (default, and what almost every DCP uses).
/// - `1` = sRGB-gamma value axis. We encode V before indexing, decode after.
pub fn apply_hue_sat_map(rgb: &mut [f32], map: &HueSatMap, value_encoding: u32) {
    if map.hue_divs == 0 || map.sat_divs == 0 || map.val_divs == 0 {
        return;
    }
    let encode = value_encoding == 1;
    rgb.par_chunks_mut(PIXELS_PER_CHUNK * 3).for_each(|chunk| {
        apply_pixels_chunk(chunk, map, encode);
    });
}

/// Per-chunk inner loop. Walks three floats at a time, runs the per-pixel
/// DCP body, writes back in place.
///
/// `#[multiversion]` makes the compiler emit a NEON (aarch64) / AVX2+FMA
/// (x86_64) copy of this function and select at runtime. The tight body
/// gets FMA-hinted lerps and `rem_euclid`-free hue wrap, so the compiler
/// has a short, straight-line per-pixel sequence with FMA throughout.
#[multiversion(targets("aarch64+neon", "x86_64+avx+avx2+fma"))]
fn apply_pixels_chunk(chunk: &mut [f32], map: &HueSatMap, encode_srgb: bool) {
    let hue_divs_f = map.hue_divs as f32;
    let sat_divs_m1 = map.sat_divs.saturating_sub(1) as f32;
    for pixel in chunk.chunks_exact_mut(3) {
        let r = pixel[0];
        let g = pixel[1];
        let b = pixel[2];
        // NaN / inf pass through unchanged — the DCP transform is undefined
        // there and callers may be carrying sentinel values.
        if !(r.is_finite() && g.is_finite() && b.is_finite()) {
            continue;
        }
        let (h_in, s_in, v_in) = rgb_to_hsv(r, g, b);

        // Early-exit on neutrals: S == 0 means hue is undefined, so only
        // the value scale applies. Avoids interpolation noise on grays.
        if s_in == 0.0 {
            let v_idx = value_index(v_in, map.val_divs, encode_srgb);
            let (_h_shift, _s_scale, v_scale) = sample_lut(map, 0.0, 0.0, v_idx);
            let v_out = v_in * v_scale;
            pixel[0] = v_out;
            pixel[1] = v_out;
            pixel[2] = v_out;
            continue;
        }

        let h_idx = h_in * hue_divs_f * (1.0 / 6.0);
        let s_idx = s_in * sat_divs_m1;
        let v_idx = value_index(v_in, map.val_divs, encode_srgb);
        let (h_shift, s_scale, v_scale) = sample_lut(map, h_idx, s_idx, v_idx);

        let h_out = wrap_hue_6(h_in + h_shift * (1.0 / 60.0));
        let s_out = (s_in * s_scale).clamp(0.0, 1.0);
        let v_out = v_in * v_scale;
        let (r_out, g_out, b_out) = hsv_to_rgb(h_out, s_out, v_out);
        pixel[0] = r_out;
        pixel[1] = g_out;
        pixel[2] = b_out;
    }
}

/// Convert linear RGB to the DNG spec's HSV form.
///
/// `H in [0, 6)` (six sectors). `S in [0, 1]`. `V = max(R, G, B)`, which can
/// exceed 1.0 after exposure lift — that's fine, the value axis is not
/// normalized.
///
/// Negative RGB (which the camera matrix occasionally produces at the gamut
/// edge) gets clamped to zero for H and S, preserving V = max(R, G, B).
#[inline(always)]
fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let v = max;
    let delta = max - min;
    if delta <= 0.0 || max <= 0.0 {
        return (0.0, 0.0, v);
    }
    let inv_delta = 1.0 / delta;
    let s = delta * (1.0 / max);
    let h = if r >= g && r >= b {
        // Between yellow and magenta. `(g - b) * inv_delta` lives in
        // `(-1, 1]`; positive arm stays, negative arm adds 6 — cheaper
        // than `rem_euclid` which calls `fmodf`.
        let hue = (g - b) * inv_delta;
        if hue >= 0.0 { hue } else { hue + 6.0 }
    } else if g >= b {
        // Between cyan and yellow — hue in (1, 3).
        (b - r).mul_add(inv_delta, 2.0)
    } else {
        // Between magenta and cyan — hue in (3, 5).
        (r - g).mul_add(inv_delta, 4.0)
    };
    (h, s, v)
}

/// Inverse of [`rgb_to_hsv`]. Unbounded V carries through.
#[inline(always)]
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    if s <= 0.0 {
        return (v, v, v);
    }
    // `h` is already in `[0, 6)` from `wrap_hue_6` above.
    let sector = h as i32; // floor for non-negative h
    let f = h - sector as f32;
    // `v * (1 - s)`, `v * (1 - s*f)`, `v * (1 - s*(1-f))` — hoisted into
    // FMA form so the compiler emits one FMADD per expression.
    let p = v.mul_add(-s, v);
    let q = v.mul_add(-s * f, v);
    let t = v.mul_add(-s * (1.0 - f), v);
    match sector {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q), // sector 5 and any rounding slop
    }
}

/// Compute the continuous value-axis index for a pixel. `val_divs == 1` is
/// a common "no value axis" profile and collapses to 0.
#[inline(always)]
fn value_index(v_in: f32, val_divs: u32, encode_srgb: bool) -> f32 {
    if val_divs <= 1 {
        return 0.0;
    }
    let v_norm = if encode_srgb {
        srgb_encode(v_in.clamp(0.0, 1.0))
    } else {
        v_in.clamp(0.0, 1.0)
    };
    v_norm * (val_divs - 1) as f32
}

/// sRGB OETF used when `ProfileHueSatMapEncoding == 1`. Applied to
/// `V = max(R, G, B)` before indexing into the value axis.
#[inline(always)]
fn srgb_encode(x: f32) -> f32 {
    if x <= 0.0031308 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// Sample the LUT with trilinear interpolation across (hue, sat, val). Each
/// axis uses the boundary rule appropriate for that axis:
///
/// - **Hue**: cyclic. `hue_divs` wraps to 0. Slides off the end rather
///   than clamping.
/// - **Sat / val**: clamped. Inputs above `sat_divs-1` / `val_divs-1` stay
///   on the last slab.
///
/// The 8-corner gather (`map.sample(..)` × 8) is intentionally scalar.
/// Gather intrinsics on aarch64 are emulated as a sequence of scalar loads
/// already, and bypassing them keeps the loads in L1 without the setup
/// cost. The 7 trilinear lerps that follow are vectorizable f32 math with
/// `mul_add` hints — that's where the SIMD win lives.
#[inline(always)]
fn sample_lut(map: &HueSatMap, h_idx: f32, s_idx: f32, v_idx: f32) -> (f32, f32, f32) {
    // Hue: wrap for the integer index. `h_idx` comes from
    // `h_in * hue_divs / 6` with `h_in in [0, 6)`, so `h_floor` is in
    // `[0, hue_divs)` already in the common case. `hue_index_wrap`
    // covers the rare rounding / above-range edge.
    let h_floor = h_idx.floor();
    let h_frac = h_idx - h_floor;
    let h0 = hue_index_wrap(h_floor as i32, map.hue_divs);
    let h1 = hue_index_wrap_next(h0, map.hue_divs);

    // Sat: clamp (non-cyclic).
    let s_max = map.sat_divs.saturating_sub(1);
    let (s0, s_frac) = clamp_axis(s_idx, s_max);
    let s1 = (s0 + 1).min(s_max);

    // Val: clamp (non-cyclic). Single-slab case: everything stays at 0.
    let v_max = map.val_divs.saturating_sub(1);
    let (v0, v_frac) = clamp_axis(v_idx, v_max);
    let v1 = (v0 + 1).min(v_max);

    // Eight corner samples — scalar gather, see function doc.
    let c000 = map.sample(h0, s0, v0);
    let c001 = map.sample(h0, s0, v1);
    let c010 = map.sample(h0, s1, v0);
    let c011 = map.sample(h0, s1, v1);
    let c100 = map.sample(h1, s0, v0);
    let c101 = map.sample(h1, s0, v1);
    let c110 = map.sample(h1, s1, v0);
    let c111 = map.sample(h1, s1, v1);

    // `lerp(a, b, t) = t * (b - a) + a`, written as one FMA.
    #[inline(always)]
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        t.mul_add(b - a, a)
    }
    #[inline(always)]
    fn lerp3(a: (f32, f32, f32), b: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
        (lerp(a.0, b.0, t), lerp(a.1, b.1, t), lerp(a.2, b.2, t))
    }

    // Reduce along v first so the cyclic hue axis collapses cleanly across
    // the 0° boundary, then s, then h.
    let h0s0 = lerp3(c000, c001, v_frac);
    let h0s1 = lerp3(c010, c011, v_frac);
    let h1s0 = lerp3(c100, c101, v_frac);
    let h1s1 = lerp3(c110, c111, v_frac);
    let h0r = lerp3(h0s0, h0s1, s_frac);
    let h1r = lerp3(h1s0, h1s1, s_frac);
    lerp3(h0r, h1r, h_frac)
}

/// Wrap a signed hue floor into `[0, hue_divs)`. The input is the floored
/// `h_idx`, which is `h_in * hue_divs / 6`. Because `h_in` sits in
/// `[0, 6)` in all reachable call sites, the value is almost always
/// already in `[0, hue_divs)`. We handle the boundary with cheap
/// conditionals instead of `rem_euclid`.
#[inline(always)]
fn hue_index_wrap(h: i32, hue_divs: u32) -> u32 {
    let divs = hue_divs as i32;
    let h = if h < 0 { h + divs } else { h };
    let h = if h >= divs { h - divs } else { h };
    (h as u32) % hue_divs
}

/// Neighbor-of hue index, cyclic. Compare-and-sub instead of generic `%`.
#[inline(always)]
fn hue_index_wrap_next(h0: u32, hue_divs: u32) -> u32 {
    let next = h0 + 1;
    if next >= hue_divs { 0 } else { next }
}

/// Clamp a floating-point axis position into `[0, max]` and split it into
/// `(integer_part, fractional_part)`. Used by [`sample_lut`] for the
/// non-cyclic axes.
#[inline(always)]
fn clamp_axis(x: f32, max: u32) -> (u32, f32) {
    let clamped = x.clamp(0.0, max as f32);
    let floor = clamped.floor();
    let frac = clamped - floor;
    (floor as u32, frac)
}

/// Wrap `h` into `[0, 6)`. Faster than `rem_euclid(6.0)` in the common
/// case where `h` is close to the range — the hue shift coming out of the
/// LUT is at most ±1 (shift / 60), and `h_in` is already in `[0, 6)`, so
/// the result of the add is in `[-1, 7)`. A pair of compare-and-adjust
/// branches covers both ends without `fmodf`.
#[inline(always)]
fn wrap_hue_6(h: f32) -> f32 {
    let h = if h < 0.0 { h + 6.0 } else { h };
    if h >= 6.0 { h - 6.0 } else { h }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_identity_map(h: u32, s: u32, v: u32) -> HueSatMap {
        let entries = (h * s * v) as usize;
        let mut data = Vec::with_capacity(entries * 3);
        for _ in 0..entries {
            data.push(0.0); // hue shift
            data.push(1.0); // sat scale
            data.push(1.0); // val scale
        }
        HueSatMap {
            hue_divs: h,
            sat_divs: s,
            val_divs: v,
            data,
        }
    }

    /// Scalar reference path for SIMD-parity tests. Mirrors the pre-6.5
    /// implementation exactly — branchy match-based HSV conversions and
    /// `rem_euclid`-based wraps — so we can diff the SIMD-friendly output
    /// against a known-good baseline.
    fn apply_scalar_reference(rgb: &mut [f32], map: &HueSatMap, value_encoding: u32) {
        if map.hue_divs == 0 || map.sat_divs == 0 || map.val_divs == 0 {
            return;
        }
        let encode = value_encoding == 1;
        for pixel in rgb.chunks_exact_mut(3) {
            let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
            if !r.is_finite() || !g.is_finite() || !b.is_finite() {
                continue;
            }
            let (h_in, s_in, v_in) = scalar_rgb_to_hsv(r, g, b);
            if s_in == 0.0 {
                let v_idx = value_index(v_in, map.val_divs, encode);
                let (_h_shift, _s_scale, v_scale) = sample_lut(map, 0.0, 0.0, v_idx);
                let v_out = v_in * v_scale;
                pixel[0] = v_out;
                pixel[1] = v_out;
                pixel[2] = v_out;
                continue;
            }
            let h_idx = h_in * (map.hue_divs as f32) / 6.0;
            let s_idx = s_in * (map.sat_divs.saturating_sub(1) as f32);
            let v_idx = value_index(v_in, map.val_divs, encode);
            let (h_shift, s_scale, v_scale) = sample_lut(map, h_idx, s_idx, v_idx);
            let h_out = (h_in + h_shift / 60.0).rem_euclid(6.0);
            let s_out = (s_in * s_scale).clamp(0.0, 1.0);
            let v_out = v_in * v_scale;
            let (r_out, g_out, b_out) = scalar_hsv_to_rgb(h_out, s_out, v_out);
            pixel[0] = r_out;
            pixel[1] = g_out;
            pixel[2] = b_out;
        }
    }

    /// Pre-6.5 scalar `rgb_to_hsv`. Kept as a test-only reference so we
    /// can assert bit-for-bit parity of the new `rgb_to_hsv`.
    fn scalar_rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let v = max;
        let delta = max - min;
        if delta <= 0.0 || max <= 0.0 {
            return (0.0, 0.0, v);
        }
        let s = delta / max;
        let h = if r >= g && r >= b {
            ((g - b) / delta).rem_euclid(6.0)
        } else if g >= b {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };
        (h, s, v)
    }

    /// Pre-6.5 scalar `hsv_to_rgb`.
    fn scalar_hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
        if s <= 0.0 {
            return (v, v, v);
        }
        let h = h.rem_euclid(6.0);
        let sector = h.floor() as i32;
        let f = h - sector as f32;
        let p = v * (1.0 - s);
        let q = v * (1.0 - s * f);
        let t = v * (1.0 - s * (1.0 - f));
        match sector {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        }
    }

    #[test]
    fn rgb_hsv_roundtrip() {
        for (r, g, b) in [
            (0.8_f32, 0.3, 0.1),
            (0.1, 0.4, 0.9),
            (0.5, 0.5, 0.5),
            (1.0, 0.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0),
            (0.7, 0.7, 0.3),
        ] {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            let (r2, g2, b2) = hsv_to_rgb(h, s, v);
            assert!(
                (r - r2).abs() < 1e-5 && (g - g2).abs() < 1e-5 && (b - b2).abs() < 1e-5,
                "roundtrip failed: ({r},{g},{b}) → ({h},{s},{v}) → ({r2},{g2},{b2})"
            );
        }
    }

    /// The FMA-ified `rgb_to_hsv` / `hsv_to_rgb` must agree with the pre-
    /// 6.5 match-based scalar reference across sector boundaries, neutrals,
    /// and above-1.0 inputs. Tolerance covers FMA rounding (one ULP max).
    #[test]
    fn simd_hsv_conversion_matches_scalar() {
        let samples: &[(f32, f32, f32)] = &[
            (0.8, 0.3, 0.1),
            (0.1, 0.4, 0.9),
            (0.5, 0.5, 0.5),
            (1.0, 0.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0),
            (0.7, 0.7, 0.3),
            (2.5, 0.1, 0.9),
            (0.01, 0.02, 0.03),
            (0.3, 0.7, 0.1),
            (0.9, 0.2, 0.9),
            (0.4, 0.6, 0.6),
            (1e-6, 1e-7, 1e-8),
        ];
        for &(r, g, b) in samples {
            let (h_f, s_f, v_f) = rgb_to_hsv(r, g, b);
            let (h_s, s_s, v_s) = scalar_rgb_to_hsv(r, g, b);
            assert!(
                (h_f - h_s).abs() < 1e-4,
                "H mismatch at ({r},{g},{b}): fast={h_f}, scalar={h_s}"
            );
            assert!(
                (s_f - s_s).abs() < 1e-5,
                "S mismatch at ({r},{g},{b}): fast={s_f}, scalar={s_s}"
            );
            assert!(
                (v_f - v_s).abs() < 1e-5,
                "V mismatch at ({r},{g},{b}): fast={v_f}, scalar={v_s}"
            );
            let (rf, gf, bf) = hsv_to_rgb(h_s, s_s, v_s);
            let (rs, gs, bs) = scalar_hsv_to_rgb(h_s, s_s, v_s);
            assert!(
                (rf - rs).abs() < 1e-5 && (gf - gs).abs() < 1e-5 && (bf - bs).abs() < 1e-5,
                "HSV→RGB mismatch for ({h_s},{s_s},{v_s}): \
                 fast=({rf},{gf},{bf}), scalar=({rs},{gs},{bs})"
            );
        }
    }

    /// End-to-end parity test: a 256×256 synthetic buffer run through the
    /// SIMD path and through the scalar reference must agree within f32
    /// FMA rounding (1e-4 absolute) on every channel.
    #[test]
    fn simd_matches_scalar_within_tolerance() {
        // Non-identity 6×6×3 HSM with small hue shift, slight sat and val
        // scaling. Exercises all three axes.
        let mut data = Vec::with_capacity(6 * 6 * 3 * 3);
        for h in 0..6 {
            for s in 0..6 {
                for v in 0..3 {
                    let hue_shift = (h as f32 - 3.0) * 4.0; // ±12°
                    let sat_scale = 1.0 + 0.1 * (s as f32 / 5.0);
                    let val_scale = 0.95 + 0.05 * (v as f32 / 2.0);
                    data.push(hue_shift);
                    data.push(sat_scale);
                    data.push(val_scale);
                }
            }
        }
        let map = HueSatMap {
            hue_divs: 6,
            sat_divs: 6,
            val_divs: 3,
            data,
        };

        let w = 256_usize;
        let h = 256_usize;
        let mut buf = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let hue = (x as f32 / w as f32) * 6.0;
                let sat = y as f32 / h as f32;
                let val = 0.2 + 0.8 * ((x + y) as f32 / (w + h) as f32);
                let (r, g, b) = scalar_hsv_to_rgb(hue, sat, val);
                buf.extend_from_slice(&[r, g, b]);
            }
        }

        let mut fast_buf = buf.clone();
        let mut scalar_buf = buf.clone();
        apply_hue_sat_map(&mut fast_buf, &map, 0);
        apply_scalar_reference(&mut scalar_buf, &map, 0);

        let mut max_diff = 0.0_f32;
        for (i, (a, b)) in fast_buf.iter().zip(scalar_buf.iter()).enumerate() {
            let d = (a - b).abs();
            if d > max_diff {
                max_diff = d;
            }
            assert!(
                d < 1e-4,
                "pixel {} channel diff {d} (fast={a}, scalar={b})",
                i / 3
            );
        }
        println!("SIMD vs scalar max channel diff on 256×256: {max_diff:.2e}");
    }

    #[test]
    fn identity_map_is_pass_through() {
        let map = make_identity_map(90, 30, 1);
        let mut pixels = vec![0.8, 0.3, 0.1, 0.1, 0.4, 0.9, 0.5, 0.5, 0.5, 0.7, 0.2, 0.9];
        let original = pixels.clone();
        apply_hue_sat_map(&mut pixels, &map, 0);
        for (i, (got, want)) in pixels.iter().zip(original.iter()).enumerate() {
            assert!(
                (got - want).abs() < 1e-5,
                "pixel[{i}] drifted: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn empty_buffer_no_panic() {
        let map = make_identity_map(2, 2, 1);
        let mut buf: Vec<f32> = Vec::new();
        apply_hue_sat_map(&mut buf, &map, 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn neutral_pixels_unchanged_under_identity() {
        // A pure gray pixel has S = 0. Our early-exit keeps H untouched, and
        // an identity val_scale leaves V the same too. So output must
        // exactly equal input.
        let map = make_identity_map(90, 30, 1);
        let mut pixels = vec![0.5, 0.5, 0.5, 0.2, 0.2, 0.2, 0.0, 0.0, 0.0];
        let original = pixels.clone();
        apply_hue_sat_map(&mut pixels, &map, 0);
        for (got, want) in pixels.iter().zip(original.iter()) {
            assert!((got - want).abs() < 1e-6);
        }
    }

    #[test]
    fn known_hue_shift_rotates_red_toward_yellow() {
        // Build a 6×1×1 LUT where every entry shifts hue by +60°. A red
        // pixel (H=0) should come out yellow (H=60° = index 1 in the
        // 6-sector form).
        let data: Vec<f32> = (0..6).flat_map(|_| [60.0_f32, 1.0, 1.0]).collect();
        let map = HueSatMap {
            hue_divs: 6,
            sat_divs: 1,
            val_divs: 1,
            data,
        };
        let mut pixels = vec![1.0_f32, 0.0, 0.0]; // pure red
        apply_hue_sat_map(&mut pixels, &map, 0);
        // After +60° shift, red becomes pure yellow: (1, 1, 0) within
        // float rounding.
        assert!((pixels[0] - 1.0).abs() < 1e-4, "R = {}", pixels[0]);
        assert!((pixels[1] - 1.0).abs() < 1e-4, "G = {}", pixels[1]);
        assert!(pixels[2].abs() < 1e-4, "B = {}", pixels[2]);
    }

    #[test]
    fn known_val_scale_doubles_brightness() {
        // Uniform val_scale of 2 across the map → every pixel's RGB
        // doubles.
        let data: Vec<f32> = (0..6).flat_map(|_| [0.0_f32, 1.0, 2.0]).collect();
        let map = HueSatMap {
            hue_divs: 6,
            sat_divs: 1,
            val_divs: 1,
            data,
        };
        let mut pixels = vec![0.4_f32, 0.2, 0.1];
        apply_hue_sat_map(&mut pixels, &map, 0);
        assert!((pixels[0] - 0.8).abs() < 1e-4, "R = {}", pixels[0]);
        assert!((pixels[1] - 0.4).abs() < 1e-4, "G = {}", pixels[1]);
        assert!((pixels[2] - 0.2).abs() < 1e-4, "B = {}", pixels[2]);
    }

    #[test]
    fn sat_scale_zero_desaturates_to_gray() {
        // sat_scale = 0 everywhere → every colored pixel collapses to its
        // value axis (gray at V).
        let data: Vec<f32> = (0..6).flat_map(|_| [0.0_f32, 0.0, 1.0]).collect();
        let map = HueSatMap {
            hue_divs: 6,
            sat_divs: 1,
            val_divs: 1,
            data,
        };
        let mut pixels = vec![0.8_f32, 0.3, 0.1];
        apply_hue_sat_map(&mut pixels, &map, 0);
        // V = 0.8, so all channels should equal 0.8.
        for (i, v) in pixels.iter().enumerate() {
            assert!((v - 0.8).abs() < 1e-4, "channel {i}: {v} vs 0.8");
        }
    }

    #[test]
    fn hue_wraparound_between_last_and_first_index() {
        // 6 hue divs. Index 5 shifts hue by +90°, index 0 shifts by -90°.
        // A pixel between sector 5 and wrap (H just below 6) should
        // interpolate across the 5→0 boundary, not between sector 5 and
        // an out-of-bounds sector 6.
        let mut data = vec![0.0_f32; 6 * 3];
        data[0] = -90.0; // hue 0 shift
        data[3] = 0.0; // hue 1
        data[6] = 0.0; // hue 2
        data[9] = 0.0; // hue 3
        data[12] = 0.0; // hue 4
        data[15] = 90.0; // hue 5 shift
        // sat and val scales all 1
        for i in 0..6 {
            data[i * 3 + 1] = 1.0;
            data[i * 3 + 2] = 1.0;
        }
        let map = HueSatMap {
            hue_divs: 6,
            sat_divs: 1,
            val_divs: 1,
            data,
        };

        // A pixel at H = 5.5 (magenta-ish) sits exactly between sector 5
        // (+90°) and sector 0 (wrapped, -90°). Trilinear at t=0.5 gives
        // shift = (90 + -90) / 2 = 0. So the hue should NOT move.
        // RGB for H=5.5 (between sector 5 and 0, i.e., between magenta
        // and red): approximate as (1, 0.5, 0).
        let mut pixels = vec![1.0_f32, 0.0, 0.5]; // red-magenta (sector 5.5)
        let (h_before, _s, _v) = rgb_to_hsv(pixels[0], pixels[1], pixels[2]);
        apply_hue_sat_map(&mut pixels, &map, 0);
        let (h_after, _s, _v) = rgb_to_hsv(pixels[0], pixels[1], pixels[2]);
        // Hue should be ~unchanged because the two shifts cancel.
        let mut hue_diff = h_after - h_before;
        // Normalize to [-3, 3] to handle wrap around if any.
        if hue_diff > 3.0 {
            hue_diff -= 6.0;
        }
        if hue_diff < -3.0 {
            hue_diff += 6.0;
        }
        assert!(
            hue_diff.abs() < 0.05,
            "hue wrap failed: before {h_before}, after {h_after}, diff {hue_diff}"
        );
    }

    #[test]
    fn nan_pixel_passes_through_untouched() {
        let map = make_identity_map(6, 1, 1);
        let mut pixels = vec![f32::NAN, 0.5, 0.5];
        apply_hue_sat_map(&mut pixels, &map, 0);
        assert!(pixels[0].is_nan(), "NaN mangled: {}", pixels[0]);
        assert!((pixels[1] - 0.5).abs() < 1e-6);
        assert!((pixels[2] - 0.5).abs() < 1e-6);
    }

    /// Bench: apply the default 90×30×1 HueSatMap to a 20-megapixel buffer
    /// and report the time. Ignored by default; run with
    /// `cargo test --release apply_hsm_bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn apply_hsm_bench() {
        let map = make_identity_map(90, 30, 1);
        let size = 5470 * 3656 * 3;
        let mut buf: Vec<f32> = (0..size).map(|i| ((i % 256) as f32) / 255.0).collect();
        // Warm up
        apply_hue_sat_map(&mut buf, &map, 0);
        let mut times = vec![];
        for _ in 0..5 {
            let t = std::time::Instant::now();
            apply_hue_sat_map(&mut buf, &map, 0);
            times.push(t.elapsed().as_millis());
        }
        println!("DCP HSM apply 20 MP (ms): {times:?}");
    }

    #[test]
    fn look_table_pass_after_hue_sat_map_darkens_target_band() {
        // Smoke test for the LookTable pipeline position: a HueSatMap that's
        // a no-op, followed by a LookTable that halves the value of the
        // red band only. A red pixel should come out half as bright; a
        // blue pixel should pass through.
        let hsm = make_identity_map(6, 1, 1);
        // 6×1×1 LookTable: index 0 (red sector) halves V, others no-op.
        let mut look_data = vec![0.0_f32; 6 * 3];
        for i in 0..6 {
            look_data[i * 3] = 0.0; // hue shift
            look_data[i * 3 + 1] = 1.0; // sat scale
            look_data[i * 3 + 2] = if i == 0 { 0.5 } else { 1.0 }; // val scale
        }
        let look = HueSatMap {
            hue_divs: 6,
            sat_divs: 1,
            val_divs: 1,
            data: look_data,
        };

        let mut pixels = vec![
            1.0_f32, 0.0, 0.0, // red
            0.0, 0.0, 1.0, // blue
        ];
        apply_hue_sat_map(&mut pixels, &hsm, 0); // no-op
        apply_hue_sat_map(&mut pixels, &look, 0); // red only

        // Red pixel halved in R; blue untouched.
        assert!((pixels[0] - 0.5).abs() < 1e-4, "red R = {}", pixels[0]);
        assert!(pixels[1].abs() < 1e-4, "red G = {}", pixels[1]);
        assert!(pixels[2].abs() < 1e-4, "red B = {}", pixels[2]);
        assert!(pixels[3].abs() < 1e-4, "blue R = {}", pixels[3]);
        assert!(pixels[4].abs() < 1e-4, "blue G = {}", pixels[4]);
        assert!((pixels[5] - 1.0).abs() < 1e-4, "blue B = {}", pixels[5]);
    }

    #[test]
    fn val_axis_single_slab_is_stable() {
        // Single-val-div profile (the common Adobe 2D case). Build a
        // 6×6×1 map with a known sat scale of 1.5.
        let mut data = vec![0.0_f32; 6 * 6 * 3];
        for i in 0..(6 * 6) {
            data[i * 3] = 0.0; // no hue shift
            data[i * 3 + 1] = 1.5; // sat *= 1.5
            data[i * 3 + 2] = 1.0; // val unchanged
        }
        let map = HueSatMap {
            hue_divs: 6,
            sat_divs: 6,
            val_divs: 1,
            data,
        };
        let mut pixels = vec![0.8_f32, 0.3, 0.1];
        apply_hue_sat_map(&mut pixels, &map, 0);
        // Sat of the input: 1 - 0.1/0.8 = 0.875. After ×1.5 clipped: 1.0.
        // V stays at 0.8. After sat=1.0, V=0.8: pure red-yellow-ish
        // primary (depending on hue).
        let (h_in, _s_in, v_in) = rgb_to_hsv(0.8, 0.3, 0.1);
        let (h_out, s_out, v_out) = rgb_to_hsv(pixels[0], pixels[1], pixels[2]);
        assert!((h_in - h_out).abs() < 1e-4, "hue shifted");
        assert!((v_in - v_out).abs() < 1e-4, "value shifted");
        assert!(
            s_out > 0.9,
            "expected sat ~1.0 after ×1.5 clamp, got {s_out}"
        );
    }
}
