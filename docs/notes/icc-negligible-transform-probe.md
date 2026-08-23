# Measuring the negligible-transform probe

`color::transform::transform_is_negligible` skips a built ICC transform when it can't move any channel of an 18³ RGB
lattice by more than 1/255. This note records what that probe was measured against. Referenced from
`src/color/CLAUDE.md`.

## Why a probe at all

M0 replaced the macOS system sRGB profile with `moxcms::ColorProfile::new_srgb().encode()`. The two are not byte-equal —
Apple's is 3,144 bytes with a 1,024-entry table TRC, ours is 612 bytes with the parametric curve — so `profiles_match`,
which is `a == b`, stopped short-circuiting for every image macOS ever tagged. Those images would otherwise pay a full
transform (42 ms on a 24 MP image, the whole navigation budget) to change nothing anyone can see.

Probing the built transform beats comparing parsed colorants and curves: there are no per-field tolerances to tune, and
it works for LUT-based profiles that no field-by-field comparison could handle.

## Method

Run the built 8-bit `Layout::Rgba` transform over **all 16,777,216 RGB triples** and take the largest per-channel
absolute difference, then compare that to what the 18³ probe lattice reports for the same transform. Source profiles
read from `/System/Library/ColorSync/Profiles/`, target is `srgb_icc_bytes()`.

## Results

| source → generated sRGB             | probe max | full-cube max | at            |
| ----------------------------------- | --------- | ------------- | ------------- |
| Apple system sRGB (1,024-entry TRC) | 1         | **1**         | (0, 0, 1)     |
| Apple Display P3                    | 105       | 118           | (118, 254, 0) |
| Apple Generic RGB (gamma 1.8)       | 64        | 64            | (249, 0, 255) |

Neutral grey 128 through each: system sRGB → 128 (unchanged), Display P3 → 128, Generic RGB → 146. The gamma-1.8 case
confirms the transform really does apply a source TRC when one differs; it is caught with a wide margin.

## What this does and doesn't establish

**The ±1/255 claim is exact.** M0's deliberate macOS output change is "colour values shift by at most 1/255 with match
display off", and the full-cube measurement confirms it over every possible pixel value, not just at the lattice points.

**The probe is not an upper bound.** The Display P3 row under-reports the true maximum by 13 out of 118, about 11%. So a
transform whose true maximum is 2 could probe as 1 and be skipped. That's the honest limit of a sampled lattice, and
it's why `NEGLIGIBLE_DELTA` stays at 1: the real guarantee is "no more than a step or two out of 255", which is below
what any display resolves, rather than a proof that nothing moves.

Raising the lattice density would narrow the gap and buy nothing — the cases that matter are two orders of magnitude
away from the threshold, and the probe already costs ~11 µs against a transform that had to be built anyway.

## A trap for whoever measures this next

Building a synthetic gamma profile with `ToneReprCurve::Parametric(vec![g])` (ICC parametric type 0, pure gamma) does
**not** work as a test input: moxcms 0.9.0 round-trips the curve through `encode()` / `new_from_slice` correctly but
ignores it when building the transform, so gamma 1.8 → gamma 2.4 comes out as identity. Real profiles ship pure gamma as
a single-entry `curv` tag, which parses as `ToneReprCurve::Lut` with one element and is applied correctly — that is what
the Generic RGB row above exercises. Use a real profile, or a multi-entry `Lut`, when checking curve handling.
