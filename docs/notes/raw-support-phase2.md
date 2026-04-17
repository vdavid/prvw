# Camera RAW support: Phase 2

Phase 1 (`raw-support-phase1.md`) wired up `rawler`'s default develop pipeline
and shipped RAW decode end to end. Phase 2 is about closing the perception gap
with Apple Photos, Affinity Photo, and Lightroom: the same RAW file decoded by
Prvw looks muddier and flatter than through those apps. That's rawler's
honest-but-plain output at work. This phase adds the viewer-polish steps that
turn a correct decode into a pleasant one.

## Goals

1. **Wide-gamut working space** — stop clipping saturated colors into sRGB
   before any corrections land.
2. **Exposure compensation** — a small digital push so landing brightness
   matches what Apple ships by default.
3. **Tone curve** — an S-curve or a look-table so midtones lift and highlights
   roll off the way a viewer expects.
4. **Light sharpening** — restore the micro-contrast Apple/Affinity bake in.

None of these require a Phase 3 rewrite. Each one is a small pass on the
developed RGB buffer.

## Test infrastructure (Step 0, this commit)

The sub-steps below will each change the pixel output. To keep honest, this
step lands first:

- A tiny synthetic Bayer DNG fixture under
  `apps/desktop/tests/fixtures/raw/synthetic-bayer-128.dng`, generated via
  `rawler::dng::writer::DngWriter`. 128×128 pixels, ~33 KB, 0BSD license.
- `color::delta_e` — CIE76 Delta-E with `DeltaEStats { mean, max, p95, count }`.
  Pure Rust, no deps.
- `synthetic_dng_matches_golden` regression test (`src/decoding/mod.rs`): runs
  `load_image` on the fixture, compares to a checked-in golden PNG with Delta-E
  thresholds of mean < 0.5 and max < 3.0. Regenerate goldens via
  `PRVW_UPDATE_GOLDENS=1 cargo test`.
- `raw-dev-dump` example (`cargo run --example raw-dev-dump -- <path>`) dumps
  labeled per-stage PNGs for visual inspection.

With that in place, each Phase 2.x sub-step can prove correctness two ways:
  1. Delta-E comparison to Apple Photos exports of the same RAW.
  2. The `raw-dev-dump` per-stage PNGs for qualitative before/after.

## Phase 2.x plan

Phases land as separate commits. Each updates the golden PNG as its final step
so future phases compare against the *new* baseline.

### Phase 2.1 — Wide-gamut working space (done, 2026-04-17)

**What changed.** The RAW decode path no longer runs rawler's full develop
pipeline. It stops after demosaic + active-area crop, then applies white
balance and the camera color matrix into a **linear Rec.2020 intermediate**
rather than into clipped sRGB. Moxcms then transforms that buffer into the
display's ICC profile in f32 — no 8-bit round trip until the very end. This
preserves every P3/Rec.2020 color the sensor captured through to the display
gate.

**Pipeline diagram.**

```
rawler::RawDevelop { Rescale, Demosaic, CropActiveArea }
  → Intermediate (camera RGB, float)
  → OUR wide-gamut step (in apps/desktop/src/decoding/raw.rs):
      * apply WB coefficients (from RawImage.wb_coeffs)
      * apply cam_to_rec2020 = invert(normalize(xyz_to_cam * rec2020_to_xyz))
      * do NOT clip; keep as f32
  → apply default crop (RawImage.crop_area)
  → color::transform_f32_with_profile(src = linear Rec.2020, dst = display_icc)
  → clamp to [0, 1], RGBA8
  → existing apply_orientation → DecodedImage
```

The matrix math is the same structure rawler uses in its own `Calibrate`
step — row-normalise then invert — only the RGB-primaries matrix changed
(sRGB → Rec.2020). That keeps neutral mapping to neutral while widening the
output gamut.

**Why Rec.2020 and not Display P3.** Rec.2020 is wider than P3 and fits
nearly every photographic color a camera sensor can capture. Picking P3
instead would still clip some saturated greens and blues on cameras with
wider native gamuts. Moxcms already ships `ColorProfile::new_bt2020()`, so
Rec.2020 costs us nothing extra: we just override the TRC to linear.

**ICC profile source.** Constructed programmatically via moxcms in
`src/color/profiles.rs::linear_rec2020_profile()`. Builds on top of
`ColorProfile::new_bt2020()` and replaces the Rec.709-parametric TRC with a
linear (empty-LUT) one. No bundled binary file, no license concern.
`linear_rec2020_icc_bytes()` is kept alongside for debug logging.

**Rec.2020 D65 → XYZ matrix.** Standard ITU-R BT.2020-2 values,
cross-checked against Bruce Lindbloom's RGB/XYZ matrix generator. Inverse
is pre-computed in `XYZ_TO_REC2020_D65` (verified by round-trip tests on
D65 whitepoint and each basis vector).

**Effect on the synthetic golden.** Delta-E ran into the 80s against the
Phase 1 golden. On inspection, that's expected drift, not a bug. The
synthetic fixture's gradient + saturated matrix produce values deep into
Rec.2020 territory. Rawler's `clip_euclidean_norm_avg` used to mix those
hypersaturated pixels toward white, giving the old golden a pink-to-white
fade. The new pipeline preserves the saturation, and moxcms' perceptual
gamut mapping lands them at the sRGB red corner for an sRGB display. On a
real photo with colors within the camera's native gamut, output is visually
identical to the Phase 1 pipeline. The golden was regenerated and visually
verified.

**Performance.** ARW decode on a 20 MP full-frame Sony file: **~115 ms**
steady-state release build on M3 Max, vs. Phase 1's ~170 ms for the develop
step alone. The new pipeline is faster because moxcms' f32 transform is
cheaper than rawler's sRGB gamma + f32→u16 conversion + our separate 8-bit
ICC transform.

**Files changed.**

- `src/color/profiles.rs` (new) — Rec.2020 matrices, `linear_rec2020_profile`,
  `linear_rec2020_icc_bytes`, matrix round-trip unit tests.
- `src/color/mod.rs` — re-exports `linear_rec2020_profile`.
- `src/color/transform.rs` — `transform_f32_with_profile` for the f32 hop.
- `src/decoding/raw.rs` — pipeline rewrite, matrix math, unit tests.
- `examples/raw-dev-dump.rs` — per-stage dumps now include `post-demosaic`,
  `post-wb`, `linear-rec2020`, and `final`.
- `tests/fixtures/raw/synthetic-bayer-128.golden.png` — regenerated.

### Phase 2.2 — Exposure compensation (TBD)

Apply a small multiplicative gain (target: +0.3 to +0.5 EV) after develop and
before tone mapping. Work in linear light; a constant scale factor on RGB
values before gamma. Land on a value that makes a set of reference RAWs match
Apple's brightness by Delta-E.

### Phase 2.3 — Tone curve (TBD)

Replace rawler's plain sRGB gamma with a filmic-style S-curve (Reinhard or
similar, maybe a camera-style LUT). This is the biggest look change. Land it
with per-fixture before/after PNGs in `docs/notes/`.

### Phase 2.4 — Light sharpening (TBD)

Unsharp mask with a small radius (0.8 pixel) and modest amount (30-50 %).
Runs on the final RGB buffer before ICC. Keep it optional if it noticeably
hurts cold-open time.

## Out of scope for Phase 2

- DNG OpcodeList1/2/3 (iPhone ProRAW gain maps). Worth doing but doesn't
  affect the core look on non-ProRAW files. Separate note when we get to it.
- DCP profiles (camera-signature looks). Lightroom territory, editor-grade.
- X-Trans Markesteijn demosaic. Viewer doesn't need this.
- Embedded-JPEG fast path. Cold-open latency optimisation, separate track.
