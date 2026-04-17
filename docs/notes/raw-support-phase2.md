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

### Phase 2.2 — Baseline exposure (done, 2026-04-17)

**What changed.** After the wide-gamut matrix and default crop, the pipeline
now applies a single EV-stop lift in linear Rec.2020 land: `linear *= 2^ev`,
per-component, Rayon-parallel. This closes most of the brightness gap
between our output and Preview.app/Affinity on real RAW files.

**Pipeline diagram (delta over Phase 2.1).**

```
... default crop
  → NEW: baseline_exposure_ev(decoder, raw)   // priority chain below
  → NEW: apply_exposure(&mut rec2020, ev)     // linear *= 2^ev
  → color::transform_f32_with_profile(...)
```

**EV source, priority order.**

1. `raw.dng_tags[BaselineExposure]` (tag 50730). Rarely populated for
   parsed DNGs but handy for files rawler built from non-DNG sources, so
   we check it first.
2. `decoder.ifd(WellKnownIFD::Root).get_entry(DngTag::BaselineExposure)` —
   reads the TIFF SRATIONAL directly off the DNG's root IFD. This is the
   path that actually fires for real DNG files.
3. Fallback: **+0.5 EV** (Adobe's neutral default, roughly what Apple
   Photos and Preview.app silently apply).

Rawler has no per-camera "baseline exposure" hint (the `Camera.hints`
field is for format-level decoder quirks, not color tuning), so we skip a
camera-hint branch.

**Safety clamp.** Output clamped to `[-2.0, +2.0]` EV and NaN/±∞ clamped to
`0.0`. Pathological DNG tags can't blow out the image.

**Real-world EV values.** Sony ARW (no DNG tag) → +0.50 EV (default). An
iPhone DNG in the `/tmp/raw/sample2.dng` bench → +0.45 EV (from the DNG
tag). Both values fall squarely in the "Adobe-neutral" band.

**Smoke-test brightness check (Sony ARW, 20 MP, mean 8-bit channel value).**
Phase 2.1 `linear-rec2020.png` preview: 62.78. Phase 2.2 `final.png`:
72.85. Apple's `sips` export of the same ARW: 74.72. We're now inside ~97 %
of Preview.app's brightness; the remaining gap closes when Phase 2.3 lands
a tone curve.

**Effect on the synthetic golden.** The gradient pixels came out brighter
(mean 8-bit RGB shifted up by the expected `1.414^(1/2.4) ≈ 1.16×` factor
for a +0.5 EV lift post-gamma). Golden was regenerated and visually
verified: same magenta-pink gradient, just brighter.

**Not configurable.** No user knob yet. Baseline exposure is part of the
default render, same as white balance or the camera matrix. If a future
phase wants a user-overridable global brightness slider, it plugs in at
the same pipeline slot; for now, we hard-code the priority chain.

**Files changed.**

- `src/decoding/raw.rs` — `baseline_exposure_ev`, `apply_exposure`,
  `baseline_exposure_ev_from_tag_value` pure helper, `tag_value_to_f32`
  converter, wired into the pipeline between crop and ICC. Unit tests
  cover each knob of the priority chain and clamp.
- `examples/raw-dev-dump.rs` — new `post-exposure.png` stage and prints
  the applied EV. Helper functions inlined to match the example's
  standalone-binary style.
- `tests/fixtures/raw/synthetic-bayer-128.golden.png` — regenerated.

### Phase 2.3 — Default tone curve (done, 2026-04-17)

**What changed.** Between the exposure lift and the ICC transform, the
pipeline now runs a mild filmic tone curve per-channel in linear Rec.2020
space. Closes the "flat look" gap against Preview.app and Affinity without
touching hue or gamut.

**Pipeline diagram (delta over Phase 2.2).**

```
... apply_exposure
  → NEW: color::tone_curve::apply_default_tone_curve(&mut rec2020)
  → color::transform_f32_with_profile(...)
```

**Curve shape — Hermite knees + lifted midtone line.** Three pieces, all
C¹-continuous at the joins:

- **Shadow knee** `[0, 0.10]` — cubic Hermite from `(0, 0)` with slope
  `1.0` (tangent to the linear reference at the origin — deep shadows
  neither crush nor lift) up to `(0.10, midtone_line(0.10))` with slope
  `1.08` (tangent to the midtone line at the knee).
- **Midtone line** `[0.10, 0.90]` — straight line with slope `1.08`
  anchored at `(0.25, 0.25)`. Picking the anchor at a low quarter tone
  (not `0.5`) puts the line *above* the diagonal across most of the
  midtone and highlight range, so the curve mostly lifts the image rather
  than darkens it. A slope of `1.08` adds mild (~8 %) midtone contrast.
- **Highlight shoulder** `[0.90, 1.0]` — cubic Hermite from `(0.90,
  midtone_line(0.90))` with slope `1.08` to `(1.0, 1.0)` with slope
  `0.30`. Values approaching 1.0 roll off gently below the midtone line's
  extension, so the curve lands on 1.0 without overshoot and without a
  hard ceiling.

**Why this shape (instead of Option B's sigmoid or Option C's LUT).** The
Hermite-with-linear-midtone formulation is analytical (no table lookup,
no root-finder), monotonic by construction for these slopes, has exact
endpoints, and fits in ~40 LoC of scalar math. Sigmoids (Option B) have
zero slopes at 0 and 1 which flatten shadows and highlights too much for
a viewer default. An Adobe-like LUT (Option C) sources cleanly but adds a
data-file ownership question we can skip by staying analytical.

**Why anchor the midtone line at 0.25 (not 0.5).** Real photos have more
content in shadows and lower midtones than in highlights. A midtone line
anchored at `(0.5, 0.5)` with slope > 1 darkens the lower half and
brightens the upper half; mean brightness drops. Anchoring at `(0.25,
0.25)` shifts the crossing with the diagonal low enough that `f(x) > x`
across most of the mid-to-upper range, matching how Preview.app and
Lightroom render linear sensor data by default.

**Safety invariants (unit-tested).** Strict monotonicity across 256
samples in `[0, 1]`; exact endpoints `f(0) == 0`, `f(1) == 1`; fixed
point `f(0.25) == 0.25`; continuity at both knees; saturation above 1.0
and clamp-to-0.0 below (including NaN). Applied per-channel in RGB, not
luminance-weighted — matches Lightroom's default and avoids hue shifts.

**Smoke-test brightness check (Sony ARW, 20 MP, mean 8-bit channel).**

| Stage                | Mean 8-bit |
|----------------------|------------|
| Phase 2.1 `linear-rec2020` preview | 62.78      |
| Phase 2.2 `post-exposure`          | 73.39      |
| Phase 2.3 `post-tone` (this)       | 73.05      |
| Phase 2.3 `final` (post-ICC)       | 72.54      |
| `sips` / Preview.app               | 74.72      |

The curve is roughly brightness-neutral on this scene (73.39 → 73.05)
while visibly adding midtone contrast and rolling off the highlight sky.
Our `final` lands at ~97 % of Preview.app's mean brightness; visually,
the contrast character is now much closer. Remaining gap is sharpening
(Phase 2.4) and per-camera tuning (Phase 3).

**Effect on the synthetic golden.** The magenta-pink gradient's
saturated reds lifted slightly (upper-midtone brightening) and saturated
highlights rolled off instead of slamming into the sRGB red corner. Mean
Delta-E against the Phase 2.2 golden was in the 20s — expected drift,
not a bug, same reasoning as Phase 2.1 (the synthetic's hypersaturated
gradient lives deep in Rec.2020 territory, so every pipeline change
moves those pixels). Golden regenerated and visually verified.

**Not configurable.** Still no user knob. Baseline render policy, same
reasoning as Phase 2.2.

**Files changed.**

- `src/color/tone_curve.rs` (new) — `apply_default_tone_curve`,
  `default_curve`, piecewise Hermite + midtone-line implementation,
  extensive unit tests (endpoints, monotonicity, knee continuity, NaN
  handling).
- `src/color/mod.rs` — exposes `tone_curve` module.
- `src/decoding/raw.rs` — pipeline wire-up between `apply_exposure` and
  the ICC transform, with a cancellation check afterwards; module doc
  lists step 4.
- `examples/raw-dev-dump.rs` — new `post-tone.png` stage between
  `post-exposure.png` and `final.png`, mirrors the curve constants for
  standalone-binary builds.
- `tests/fixtures/raw/synthetic-bayer-128.golden.png` — regenerated.

### Phase 2.4 — Capture sharpening (done, 2026-04-17)

**What changed.** After the ICC transform lands pixels in display-space
RGBA8, the pipeline applies a mild unsharp mask: `output = original +
(original - blurred) * 0.3`. Blur is a separable 1D Gaussian (σ = 0.8 px,
7 taps), run horizontally then vertically. Parallel over rows via Rayon.
Closes the "slightly soft" gap against Preview.app and Lightroom, both of
which apply similar capture sharpening silently on open.

**Pipeline diagram (delta over Phase 2.3).**

```
... color::tone_curve::apply_default_tone_curve
  → color::transform_f32_with_profile(..., target ICC)
  → rec2020_to_rgba8 (f32 → clamped RGBA8)
  → NEW: color::sharpen::sharpen_rgba8_inplace
  → apply_orientation (unchanged)
```

**Option B (post-ICC, display-space RGB8) over Option A (pre-ICC,
linear Rec.2020 f32).** We considered both:

- **A.** Conceptually cleaner (aesthetic processing in linear space before
  gamut conversion). Cheap to run on f32.
- **B.** Matches Lightroom/Camera Raw's own slot. Avoids the "over-sharpened
  halos on bright edges" failure mode linear-space unsharp produces — the
  subtraction has more headroom on the linear side, so edges that look
  fine in gamma-encoded preview read as over-brightened on a display.

We picked **B**. Rationale: 8-bit quantisation is below one gray level at
our modest amount (≤ 0.3), and matching the perceptual response of the
final gamma-encoded buffer is the primary goal of capture sharpening. It's
also the last pre-orientation step, so we never sharpen pixels we're
about to throw away.

**Parameters.** Baked in, no user knob:

- **Radius σ = 0.8 px.** Small enough to target fine detail (grass, bark,
  fabric) without chasing wide edges that'd produce halos. Kernel sized
  to `2 × ceil(3σ) + 1 = 7` taps.
- **Amount = 0.3.** Mild. On the Sony ARW test image, Laplacian edge
  energy jumps from 5.39 (post-ICC) to 6.39 (post-sharpen), a +18 %
  crispness bump. Amount 0.4 lifts that another ~15 % but reads as
  over-sharpened next to Preview.app at 1:1 zoom; 0.2 is under the
  Preview.app crispness floor.
- **Threshold = 0.** No edge discrimination. Noise reduction is out of
  scope for a viewer.

**Edge handling.** Clamp-to-edge replication. Simpler than reflection and
visually indistinguishable for a 7-tap kernel.

**Safety invariants (unit-tested).** Flat buffers pass through unchanged
(no edges → no sharpening); overshoot at bright edges saturates at 255
rather than wrapping; undershoot at dark edges clamps at 0; alpha bytes
are never read or written; output dimensions equal input dimensions;
Gaussian kernel sums to 1.0 and is symmetric.

**Effect on the synthetic golden.** Almost nothing. The fixture's
magenta-pink gradient has no edges worth sharpening, so the regenerated
golden differs from Phase 2.3's only in a handful of near-endpoint
pixels (Delta-E well below the mean < 0.5 threshold). As expected.

**Smoke-test measurements (Sony ARW, 20 MP).** `post-icc` vs. `final`
snapshots from `raw-dev-dump`:

| Metric                      | post-icc          | final (post-sharpen) | Preview.app (sips) |
|----------------------------|-------------------|----------------------|--------------------|
| Mean R, G, B (8-bit)       | 72.18, 74.59, 70.85 | 72.19, 74.59, 70.85 | 75.48, 76.97, 72.17 |
| Laplacian edge energy      | 5.39              | 6.39                 | 4.59               |
| Mean brightness ratio vs Preview | 96.8 %       | 96.8 %               | 100 %              |

Sharpening is brightness-neutral at the mean (good — unsharp mask's
`original - blurred` integrates to zero over a flat integral). Edge
energy rises by ~18 %. `sips` renders conservatively (its own export
does not appear to apply capture sharpening), so our final crispness
sitting above `sips` is expected; Preview.app on-screen renders closer
to our `final`.

**Perf.** Isolated 20 MP sharpen on this dev machine: **58–73 ms** in
release mode with Rayon. End-to-end Sony ARW decode: **217–282 ms**
across five runs (vs. ~160 ms pre-sharpen). The sharpen adds ~60 ms,
slightly above the 50 ms guidance but well under 100 ms. Further
micro-optimisation (SIMD on the inner tap loop, tile-based cache
locality) is tracked as a Phase 3 follow-up; for a viewer-grade default
the current perf is acceptable.

**Not configurable.** Consistent with exposure and tone-curve defaults.
A future user knob would slot in at the same pipeline step.

**Files changed.**

- `src/color/sharpen.rs` (new) — `sharpen_rgba8_inplace`, separable
  Gaussian blur, unsharp combine, with extensive unit tests.
- `src/color/mod.rs` — exposes `sharpen` module.
- `src/decoding/raw.rs` — pipeline wire-up after `rec2020_to_rgba8`,
  with cancellation checks on both sides; module doc lists step 5.
- `examples/raw-dev-dump.rs` — new `post-icc.png` and `final.png`
  stages (renamed from the old single `final.png`), mirror-constants
  for standalone build.
- `tests/fixtures/raw/synthetic-bayer-128.golden.png` — regenerated.

### Phase 2.5a — luminance-only tone + sharpen + saturation boost (done, 2026-04-17)

**What changed.** The Phase 2.3 tone curve and the Phase 2.4 sharpening both
ran per-channel. Per-channel math has two structural problems:

- A per-channel tone curve desaturates colors near the highlight shoulder.
  A pixel like `(R=1.0, G=0.9, B=0.6)` sees each channel compressed
  independently, so the three channels get pushed toward one another. The
  `R:G:B` ratio changes and chroma collapses.
- Per-channel sharpening produces color fringes at colored edges, because
  each channel amplifies its own edge and the three edges don't line up at
  sub-pixel precision.

Phase 2.5a keeps every parameter value unchanged (midtone anchor 0.25,
sharpen amount 0.3) and fixes these two issues structurally by moving both
passes to **luminance only**, then adds a mild **global saturation boost**
to approximate the "vibrancy" Apple and Affinity bake into their per-camera
tuning.

**Pipeline diagram (delta over Phase 2.4).**

```
... apply_exposure
  → color::tone_curve::apply_default_tone_curve (NOW luminance-only)
  → NEW: color::saturation::apply_saturation_boost(&mut rec2020, 0.08)
  → transform_f32_with_profile(..., target ICC)
  → rec2020_to_rgba8
  → color::sharpen::sharpen_rgba8_inplace (NOW luminance-only)
  → apply_orientation
```

**Luminance-only pattern.** Same math applies to any scalar-on-Y operation:

```
Y_in   = luma_weighted_sum(R, G, B)
Y_out  = operation(Y_in)                      // tone curve, or unsharp-mask
scale  = Y_out / max(Y_in, epsilon)           // epsilon guards div-by-zero
R_out  = R_in * scale
G_out  = G_in * scale
B_out  = B_in * scale
```

This preserves hue (the `R:G:B` ratio stays identical) and scales saturation
uniformly with brightness. Near-black pixels (below `DARK_EPSILON`) are
zeroed out for the tone curve and passed through for the sharpen, both to
dodge the division blow-up.

**Color spaces and luma weights.**

- Tone curve runs in **linear Rec.2020** (pre-ICC), so we use the
  **Rec.2020 luma weights** from ITU-R BT.2020-2 §5:
  `Y = 0.2627 R + 0.6780 G + 0.0593 B`.
- Sharpening runs in **display RGB8** (post-ICC), so we use the
  **Rec.709 / sRGB luma weights**:
  `Y = 0.2126 R + 0.7152 G + 0.0722 B`. Close enough to Display P3's own
  weights (~0.228 R) that the ~2 % mismatch is irrelevant at our small
  sharpen amplitude.

**Saturation boost.** Classic "scale chroma around luma" formula applied
after the tone curve and before the ICC transform:

```
Y     = luma(R, G, B)
R_out = Y + (R - Y) * (1 + boost)
G_out = Y + (G - Y) * (1 + boost)
B_out = Y + (B - Y) * (1 + boost)
```

Preserves hue and luminance exactly; only chroma scales. We apply in
**linear Rec.2020** rather than post-ICC because chroma math is most
perceptually uniform in linear space — a +8 % scale on `(R − Y)` lifts
shadows and highlights by the same visual amount, while doing the same
operation on gamma-encoded RGB8 pushes shadows harder than highlights
because of the gamma curve.

Default boost is **+8 %** (`DEFAULT_SATURATION_BOOST = 0.08`). That's the
smallest value that noticeably closes the vibrancy gap against Preview.app
and Affinity on real photos without reading as "over-processed." The value
is a structural default for Phase 2.5a; a separate Phase 2.5b pass will
grid-search it against reference output.

**Sharpening approach — Y-scale rather than YCbCr.** We blur the luminance
plane (f32) and reconstruct RGB by scaling each pixel's existing `(R, G, B)`
by `Y_out / Y_in`. Cleaner to implement than a full YCbCr round-trip and
has the same end result for a scalar edge-boost operation. f32 intermediate
math avoids the quantisation noise of doing the unsharp mask in u8.

**Synthetic golden.** Regenerated. Same magenta-pink gradient, with
subtly higher saturation — visually verified against the Phase 2.4 golden.

**Smoke test (Sony ARW, 20 MP, mean 8-bit channel value).**

| Stage                      | Mean R, G, B     | Notes                          |
|----------------------------|-------------------|--------------------------------|
| Phase 2.5a post-exposure   | 73.56, 74.93, 71.69 | same as Phase 2.4           |
| Phase 2.5a post-tone       | 73.16, 74.43, 71.34 | brightness-neutral, chroma kept |
| Phase 2.5a post-saturation | 73.07, 74.46, 70.98 | luminance preserved, chroma up |
| Phase 2.5a post-icc        | 71.95, 74.57, 70.22 |                                |
| Phase 2.5a final           | 71.97, 74.56, 70.20 | sharpen brightness-neutral     |
| `sips` / Preview.app       | 75.06, 76.90, 71.65 | reference                      |

Sharpen is brightness-neutral at the mean (delta < 0.03 on every channel),
exactly as the Y-only math predicts. Saturation boost preserves luminance
within float rounding. Visually on the ARW: green shirt and grass show
the expected vibrancy bump; no color fringes land at edges of the tree
silhouette; no magenta shifts near the bright sky. CIE76 Delta-E vs. `sips`
mean is 11.57, expected given `sips`' conservative rendering and that we
now hold chroma stronger.

**Perf.** Sony ARW 20 MP end-to-end decode: **177–223 ms** (steady-state
~180 ms) on an M3 Max, vs. Phase 2.4's 217–282 ms. The luminance-only
sharpen is actually **faster** than per-channel because it only runs the
separable blur on one plane instead of three. Saturation boost adds a few
ms of parallel scalar math. No regression.

**Files changed.**

- `src/color/tone_curve.rs` — `apply_default_tone_curve` now shapes
  luminance only, scales RGB by `Y_out / Y_in`. Unit tests added for
  neutral-gray-at-anchor invariance, pure-primary hue preservation,
  mixed-pixel `R:G:B` ratio preservation, near-black pixel guard, NaN
  guard. Scalar `default_curve` unchanged.
- `src/color/saturation.rs` (new) — `apply_saturation_boost`,
  `DEFAULT_SATURATION_BOOST = 0.08`. Tests: zero-boost no-op, neutral gray
  unchanged, pure primary retains hue, chroma grows by `(1 + boost)`,
  luminance preserved, negative boost desaturates.
- `src/color/sharpen.rs` — `sharpen_rgba8_inplace` now computes a single
  luminance plane, blurs it, applies the unsharp-mask formula on Y, and
  reconstructs RGB via the Y-scale pattern. Colored-edge hue-preservation
  test added.
- `src/color/mod.rs` — exposes `saturation` module.
- `src/decoding/raw.rs` — saturation boost wired between the tone curve
  and the ICC transform, with a cancellation check. Module doc updated
  to list six steps (tone, saturation, ICC, sharpen, orientation).
- `examples/raw-dev-dump.rs` — added `post-saturation.png` stage between
  `post-tone.png` and `post-icc.png`. Mirrored the luminance-only
  tone curve, saturation boost, and luminance-only sharpen.
- `tests/fixtures/raw/synthetic-bayer-128.golden.png` — regenerated.

**Parameters intentionally unchanged this commit.** Midtone anchor stays
at 0.25. Sharpen amount stays at 0.3. A separate Phase 2.5b agent will
grid-search all three (midtone anchor, sharpen amount, saturation boost)
against reference output and pick empirically.

### Phase 2.5b — empirical parameter tuning (done, 2026-04-17)

**Goal.** Replace the educated-guess defaults from Phase 2.5a with
empirically-tuned values. Grid-search the three knobs (tone curve midtone
anchor, sharpen amount, saturation boost) against `sips` reference
output, cross-validated across three RAW files.

**Tool.** `apps/desktop/examples/raw-tune.rs` (new). Decode-once /
tune-many: the expensive stages (rawler demosaic, WB + camera matrix,
exposure, default crop) don't depend on the tuned parameters, so we run
them once per input and sweep only the cheap post-tone stages. ~150 ms
per combo × file on an M3 Max. Supports multiple `--raw`/`--reference`
pairs for cross-validation; ranks combos by mean-of-means Delta-E across
every input.

**Structural refactor.** Promoted the module constants to named
`DEFAULT_*` values with parametric apply functions next to the existing
default wrappers:

- `color::tone_curve::apply_tone_curve(rgb, midtone_anchor)` +
  `DEFAULT_MIDTONE_ANCHOR` const. `apply_default_tone_curve` stays as
  the production entry point.
- `color::sharpen::sharpen_rgba8_inplace_with(rgba, w, h, sigma, amount)`
  + `DEFAULT_SIGMA` / `DEFAULT_AMOUNT`. `sharpen_rgba8_inplace` stays
  as the default.
- `color::saturation` already took `boost` as a parameter; no change
  needed beyond docs.

The production pipeline still goes through the `apply_default_*` and
`sharpen_rgba8_inplace` wrappers, so nothing changes at runtime. The
refactor sets up Phase 3's per-camera DCP profile work — Lightroom's
per-camera profiles vary the tone curve subtly, and we'll want to plumb
those values through the same parametric apply.

**Reference set.** Three RAW files, all on this dev machine (not in the
repo — the Phase 3 agent will wire up a committed fixture). All three
references are Apple `sips -s format png` exports:

| File                     | Dimensions | Scene                                   |
|--------------------------|------------|-----------------------------------------|
| `/tmp/raw/sample1.arw`   | 5456×3632  | Sony ARW, silhouetted trees on bright sky (limited skin tones, muted palette) |
| `/tmp/raw/sample2.dng`   | 3990×3000  | iPhone DNG, ProRAW, vertical portrait   |
| `/tmp/raw/sample3.arw`   | 5456×3632  | Sony ARW, different outdoor scene       |

**Grid searched.**

- Tone curve midtone anchor: 0.25, 0.30, 0.35, 0.40, 0.45, 0.50
- Sharpen amount: 0.30, 0.35, 0.40, 0.45, 0.50, 0.55
- Saturation boost: 0.00, 0.04, 0.08, 0.12, 0.16, 0.20

216 combos. Ranked by mean Delta-E across the three files
(mean-of-means).

**Top 10 (from `cross-full/cross-validation.csv`).**

| rank | anchor | amount | boost | mean-of-m | sample1 | sample2 | sample3 |
|------|--------|--------|-------|-----------|---------|---------|---------|
| 1    | 0.25   | 0.30   | +0.08 | 11.442    | 11.544  | 6.238   | 16.544  |
| 2    | 0.25   | 0.30   | +0.04 | 11.444    | 11.440  | 6.165   | 16.728  |
| 3    | 0.25   | 0.35   | +0.08 | 11.445    | 11.548  | 6.241   | 16.546  |
| 4    | 0.25   | 0.35   | +0.04 | 11.447    | 11.444  | 6.168   | 16.729  |
| 5    | 0.25   | 0.40   | +0.08 | 11.447    | 11.551  | 6.244   | 16.547  |
| 6    | 0.25   | 0.40   | +0.04 | 11.450    | 11.447  | 6.171   | 16.731  |
| 7    | 0.25   | 0.45   | +0.08 | 11.450    | 11.554  | 6.248   | 16.548  |
| 8    | 0.25   | 0.50   | +0.08 | 11.452    | 11.556  | 6.251   | 16.549  |
| 9    | 0.25   | 0.45   | +0.04 | 11.452    | 11.450  | 6.175   | 16.732  |
| 10   | 0.25   | 0.55   | +0.08 | 11.454    | 11.559  | 6.254   | 16.550  |

Full rank range across the 216-combo grid: **11.442** (rank 1) to
**12.418** (rank 216). Spread ~1.0 Delta-E, mostly driven by anchor —
rank 1 is always anchor 0.25 in this grid, and anchor 0.50 lands near
the bottom.

**Winning combo.** `anchor=0.25, amount=0.30, boost=+0.08` — the
Phase 2.5a "educated-guess" defaults. **No change to the production
constants.**

**Before/after on sample1.arw.** Baseline mean Delta-E: 11.57 (Phase
2.5a). Winning combo mean Delta-E: 11.544. A 0.03 improvement — below
the perceptible threshold (~1.0). Across all three files the winning
combo is at the bottom of a very flat basin. Every combo in the top 10
is within 0.012 Delta-E of the winner.

**Per-file sub-optima.** Each file has a slightly different preferred
combo when ranked on its own, but they all point at `anchor=0.25`:

| file     | best combo                      | mean dE |
|----------|---------------------------------|---------|
| sample1  | anchor 0.25, amount 0.30, boost +0.00 | 11.34   |
| sample2  | anchor 0.50, amount 0.55, boost +0.00 | 6.09    |
| sample3  | anchor 0.25, amount 0.55, boost +0.16 | 16.39   |

sample1 wants less saturation boost. sample3 wants more. sample2 is a
different scene type (iPhone ProRAW, vertical portrait) and lands well
lower in Delta-E overall (6.09 vs 11.34 and 16.39). The mean-of-means
winner splits the difference at boost +0.08.

**Wider-grid probe.** After the main grid confirmed a 0.25-anchor
winner, we also probed `anchor ∈ {0.05, 0.10, 0.12, 0.15, 0.18}`.
Lowest mean-of-means landed at anchor 0.10 (11.301) — a further ~0.14
improvement vs. the 0.25-anchor winner. **We did not ship that value.**
Reasoning:

1. The improvement is well below the "barely perceptible" Delta-E 1.0
   threshold. Every anchor in `[0.05, 0.25]` lands in the same flat
   basin.
2. `sips` is known to render more conservatively than Preview.app
   (the Phase 2.4 agent measured `sips` Laplacian 4.6 vs. Preview.app
   on-screen ~6.4). Pushing our tone curve toward `sips`'s look fits
   to `sips` specifically, not to "a viewer default human eyes expect".
3. The Phase 2.5a rationale for anchoring at 0.25 was that real photos
   have more content in shadows and lower midtones; anchor 0.25 lifts
   the mid-to-upper range across the diagonal. Lower anchors (0.10,
   0.05) would over-lift and could look "washed out" on scenes with
   already-bright subjects.
4. Phase 3's DCP profile work will bring per-camera curves. That's the
   cleaner place to pick scene-specific tone, rather than shifting the
   single global anchor to match `sips` on three files.

**Scene-dependence observed.** The three files disagree on optimal
saturation boost (sample1 prefers 0.00, sample3 prefers 0.12–0.16, with
sample2 happiest at 0.00). This confirms what Phase 3's DCP profile
work is meant to handle: scene- and sensor-specific tone + saturation
tuning. The single-global-default is a compromise; DCP gives us a
per-camera LUT, which will move the needle further than any tuning of
a global default can.

**Caveats documented.**

- The ARW references (sample1, sample3) are silhouette/outdoor scenes
  with muted palettes. A portrait with normal skin tones would exercise
  the tone curve and saturation boost differently; re-running the grid
  when a vendor RAW fixture lands (Phase 3 ancillary) is advisable.
- `sips` is the closest automated proxy for "Apple's look" but not a
  perfect one. It skips capture sharpening that Preview.app shows
  on-screen. Our sharpen amount 0.30 is at the floor of the grid (we
  intentionally didn't go below — anything lower would fit to `sips`'s
  unsharpened export rather than Preview.app's on-screen rendering).
- Cross-validation uses three files, two of which are the same camera
  (Sony). More camera diversity in the reference set would catch
  per-camera quirks that a global tune can't see.

**Files changed.**

- `apps/desktop/examples/raw-tune.rs` (new) — the grid-search tool.
  Decodes once per input, sweeps combos in parallel, ranks by
  mean-of-means Delta-E, writes per-file `best-<stem>.png` and a
  `cross-validation.csv`. Mirrors the tone/saturation/sharpen math
  inline (same reason as `raw-dev-dump.rs`).
- `apps/desktop/src/color/tone_curve.rs` — added
  `pub const DEFAULT_MIDTONE_ANCHOR` + `pub fn apply_tone_curve(rgb,
  midtone_anchor)` + `pub fn curve(x, midtone_anchor)`. The existing
  `apply_default_tone_curve` and `default_curve` stay as the default
  wrappers. A `parametric_curve_anchors_at_supplied_value` test
  verifies the curve still passes through `(anchor, anchor)` for
  every anchor in the grid.
- `apps/desktop/src/color/sharpen.rs` — renamed `SIGMA` / `AMOUNT` to
  `pub const DEFAULT_SIGMA` / `DEFAULT_AMOUNT`. Added
  `pub fn sharpen_rgba8_inplace_with(rgba, w, h, sigma, amount)` as
  the parametric entry point; `sharpen_rgba8_inplace` delegates.
- `apps/desktop/src/color/saturation.rs` — wording polish only; the
  module already took `boost` as a parameter.
- No change to `raw.rs`: pipeline stays on the default apply wrappers.
- No change to the synthetic-DNG golden: defaults are unchanged, so
  the regenerated golden is byte-identical to Phase 2.5a's.

## Summary — Phase 2 wrap-up

Phase 2 closed the four gaps between rawler's sensor-honest output and
what viewers like Preview.app and Apple Photos ship:

```
rawler::RawDevelop { Rescale, Demosaic, CropActiveArea }
  → camera_to_linear_rec2020  (WB + cam → linear Rec.2020, no clip)
  → apply_default_crop
  → apply_exposure            (+0.5 EV default, or DNG BaselineExposure tag)
  → apply_default_tone_curve  (Hermite knees + midtone line, anchor at 0.25)
  → transform_f32_with_profile (linear Rec.2020 → display ICC, in f32)
  → rec2020_to_rgba8          (clamp + quantise)
  → sharpen_rgba8_inplace     (σ 0.8 px, amount 0.3, unsharp mask)
  → apply_orientation
```

**Sony ARW measured deltas (20 MP, mean 8-bit channel).**

| Stage                        | Mean R, G, B (8-bit) | Laplacian |
|------------------------------|----------------------|-----------|
| Phase 2.1 linear-Rec.2020     | 62.78 (single channel quoted earlier) | – |
| Phase 2.2 post-exposure       | 73.39                | – |
| Phase 2.3 post-tone           | 73.05                | – |
| Phase 2.3 post-ICC            | 72.18, 74.59, 70.85 | 5.39 |
| **Phase 2.4 final**           | **72.19, 74.59, 70.85** | **6.39** |
| sips / Preview.app export     | 75.48, 76.97, 72.17 | 4.59 |

Brightness lands at ~97 % of Preview.app's `sips` export (note: `sips`
itself renders conservatively; Preview.app on-screen is brighter still).
Tone-curve contrast punch and highlight roll-off match Preview.app's feel.
Sharpening closes the crispness gap without pushing into halo territory.

**Perf.** Sony ARW 20 MP decode on this dev machine: **~220–280 ms**
release-build end-to-end. Phase 1's develop step alone was ~170 ms on
an M3 Max. Phase 2's extra stages (wide-gamut matrix, exposure, tone,
sharpen) each cost a few tens of ms, but the switch to f32 ICC is
faster than Phase 1's f32→u16→ICC path, so total time isn't dramatically
worse for a much better picture.

**What's next (Phase 3 hints).** Remaining gaps where Preview.app /
Lightroom still pull ahead on specific files:

- **DNG OpcodeList2/3 application.** iPhone ProRAW gain maps, corner
  shading, and lens corrections sit in these opcodes. Rawler parses them
  but doesn't apply them. Visible on iPhone and some mirrorless RAWs.
- **Per-camera tone/baseline tuning.** Adobe ships DNG camera-profile
  tables (DCP files) per body. Lightroom's "Camera Standard" profile
  varies the tone curve subtly per sensor. A DCP parser + applier is a
  Phase 3 target.
- **Highlight recovery.** Today we clip at the matrix/ICC stage. Real
  recovery blends the two unsaturated channels into the clipped one.
  Separate pass, linear-space.
- **Sharpening inner-loop SIMD.** The current implementation is
  Rayon-parallel but not SIMD-vectorised. A ~3× speedup is plausible
  with explicit NEON/SSE taps for the kernel convolution.

## Out of scope for Phase 2

- DNG OpcodeList1/2/3 (iPhone ProRAW gain maps). Worth doing but doesn't
  affect the core look on non-ProRAW files. Separate note when we get to it.
- DCP profiles (camera-signature looks). Lightroom territory, editor-grade.
- X-Trans Markesteijn demosaic. Viewer doesn't need this.
- Embedded-JPEG fast path. Cold-open latency optimisation, separate track.
