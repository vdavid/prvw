# RAW support — Phase 6

Phase 6 opens the RAW pipeline's tuning knobs to the user through the
Settings → RAW → Tuning section. The goal: let users nudge the parametric
stages (sharpening, saturation, tone curve shape) without editing JSON by
hand or rebuilding the binary.

## Phase 6.0 — user-facing tuning sliders (shipped 2026-04-17)

### What shipped

Three NSSlider widgets live under a new "Tuning" section in Settings →
RAW, wedged between the existing "Output" toggle and the "DCP profile"
row. Each drives one `f32` field on `RawPipelineFlags`:

| Slider              | Field                  | Range         | Step | Default            | Drives                                           |
| ------------------- | ---------------------- | ------------- | ---- | ------------------ | ------------------------------------------------ |
| Sharpening amount   | `sharpen_amount`       | 0.00 – 1.00   | 0.05 | `DEFAULT_AMOUNT` (0.30) | `color::sharpen::sharpen_rgba{8,16f}_inplace_with` |
| Saturation boost    | `saturation_boost_amount` | 0.00 – 0.30 | 0.01 | `DEFAULT_SATURATION_BOOST` (0.08) | `color::saturation::apply_saturation_boost` |
| Tone midtone anchor | `midtone_anchor`       | 0.20 – 0.50   | 0.01 | `DEFAULT_MIDTONE_ANCHOR` (0.40) | `color::tone_curve::apply_tone_curve`            |

Defaults match the constants used before Phase 6.0, so a user who leaves
the sliders alone sees bit-identical output. `RawPipelineFlags::clamp_knobs`
runs once per decode inside `raw.rs` and pulls hand-edited out-of-range
values back into range without rejecting the whole settings file.

### Why these three

I looked at every parametric knob the RAW pipeline touches today and
picked the three with the highest taste-to-risk ratio. Everything else
stays internal.

- **Sharpening amount, not σ.** Phase 2.4's Laplacian measurement showed
  amount dominates perceived crispness; σ (the Gaussian blur radius)
  trades halos for softness but the window between "blurry" and "haloed"
  is narrow. Exposing σ as a second slider would invite users into the
  bad-settings range where it's easy to make the image look worse. The
  production σ (`DEFAULT_SIGMA = 0.8 px`) stays fixed.
- **Saturation boost, not ProcessingStyle / DCP injection.** Users who
  want warm / cool / skin-tone shifts reach for a DCP profile or an
  editor, not a viewer. The one global-chroma knob handles "too muted"
  and "too much pop" preferences, which is what Preview.app / Photos
  users actually want to tune.
- **Midtone anchor, not filmic peak.** Peak is a display decision
  (`DEFAULT_PEAK_SDR = 1.0`, `DEFAULT_PEAK_HDR = 4.0` per Phase 5) —
  users don't have a taste opinion on highlight asymptote height.
  Anchor is a straightforward "brighter midtones vs. darker midtones"
  knob that everyone understands.

### UI decisions

- **Discrete commits, live label updates deferred.** `setContinuous(false)`
  means AppKit fires the slider action exactly once on mouse release. A
  single drag = a single decode. We considered live-updating the numeric
  label during drag (via `currentEvent.type` inspection or a separate
  tracking delegate) but shipped the simpler version — value clarity on
  release is enough, and avoiding decode-spam matters more on 20 MP RAWs
  where each decode costs tens of milliseconds.
- **Numeric label to 2 decimals.** All three ranges are narrow enough
  (0.00 – 1.00 at worst) that 2 decimals convey the full usable
  resolution without noise. The saturation range tops out at 0.30 so
  "0.08" and "0.12" read cleanly.
- **Slider minimum width = 160 px.** Without an explicit minimum the
  slider track collapses when the panel is narrow and long row titles
  "win" the stack-view space negotiation.
- **Ivar pointers + raw struct init.** Same pattern as the existing
  `RawDelegateIvars` — pointers into `retained_views` survive for the
  window's lifetime, so the delegate can read slider state without
  fighting Rust borrow rules across AppKit dispatch. Three slider
  pointers + three value-label pointers were added alongside the
  existing toggle pointers.
- **Reset to defaults covers sliders too.** The original
  `write_flags_to_switches` was renamed to `write_flags_to_all_widgets`
  and now covers sliders + value labels. A reset click snaps the full
  UI back in one step, matching the existing toggle behavior.

### Persistence

Each of the three floats carries a `#[serde(default = "...")]` pointing
at the corresponding constant, so older settings.json files (missing
the new keys) load silently without losing the user's other prefs. Two
round-trip tests pin this down: one at the `RawPipelineFlags` level
(`round_trip_preserves_values`, `round_trip_preserves_float_precision`
in `raw_flags.rs`), one at the outer `Settings` level
(`round_trip_preserves_raw_tuning_knobs` in `persistence.rs`).

### What's next

Phase 6.0 is feature-complete. Further knob exposure (per-image DCP
override, per-lens LensFun override, custom curves) would be Phase 6.1+
and isn't planned yet. A user requesting more control than the three
current sliders can always edit `settings.json` directly — the clamp
protects against out-of-range values either way.

## Phase 6.1 — chroma noise reduction (shipped 2026-04-17)

Preview.app and Affinity Photo silently apply mild chroma NR by default;
we didn't, and it showed as the visible quality gap on high-ISO shots.
Phase 6.1 closes that gap.

### Algorithm

Chroma-only spatial blur that keeps luminance sharp. For each pixel:

1. Convert linear Rec.2020 RGB to Y + Cb + Cr using Rec.2020 weights
   (`Y = 0.2627 R + 0.6780 G + 0.0593 B`, `Cb = B − Y`, `Cr = R − Y`).
2. Run a separable Gaussian blur on the Cb plane, then on the Cr plane.
3. Reconstruct: `R = Y + Cr`, `B = Y + Cb`,
   `G = (Y − 0.2627 R − 0.0593 B) / 0.6780`.

Because blurring only happens on Cb and Cr, luma is preserved per-pixel
(within f32 rounding). A unit test pins this down on a synthetic image:
reading `Y = LUMA_R · R + LUMA_G · G + LUMA_B · B` before and after the
pass yields the same number.

### Parameters

- **Sigma = 1.5 px.** Mild, matching the consumer-viewer default. Smaller
  σ (~1.0) cleans less; larger σ (~3.0) starts smearing colored edges.
  Kernel is `2 · ceil(3σ) + 1 = 11` taps.
- **Strength = 1.0.** v1 exposes the stage as an on/off toggle in
  Settings → RAW → "Denoise". Partial-strength mixing is wired in the
  module API (`apply_chroma_denoise`) but not exposed as a slider yet;
  future Phase 6.x work can reach it without reshaping signatures.

### Pipeline slot

Linear Rec.2020, post-demosaic and post-`apply_default_crop`, immediately
before the baseline exposure lift. Chroma noise is rawest closest to
demosaic output, so cleaning it there is cheapest and least destructive.
Later stages scale luminance (exposure) or shape it (tone curve /
saturation boost), neither of which can re-introduce chroma noise.

### Default-on behavior change

`chroma_denoise` defaults to `true`. This intentionally changes output
for new decodes vs. pre-6.1. Per-image behavior at `chroma_denoise =
false` stays bit-identical to pre-6.1. Same pattern as the HDR output
toggle from Phase 5.

The `synthetic_dng_matches_golden` test regenerated its golden PNG with
chroma denoise on. The synthetic Bayer fixture only has one color
boundary, so the delta is visually imperceptible — which is the correct
sanity check: a "broken" regeneration would look wildly different.

### Performance

Measured on an Apple M-series laptop, SDR RGBA8 path, release build,
averaged over 3 iterations each (after a warm-up):

| File          | Resolution | Pre-6.1 | With 6.1 | Delta   |
| ------------- | ---------- | ------- | -------- | ------- |
| sample1.arw   | 5456 × 3632 (~20 MP) | 275 ms | 334 ms | +58 ms |
| sample2.dng   | 3000 × 3990 (~12 MP) | 252 ms | 277 ms | +25 ms |
| sample3.arw   | 5456 × 3632 (~20 MP) | 269 ms | 342 ms | +72 ms |

The isolated 20 MP chroma-denoise pass benchmarks at ~55 ms in
`color::chroma_denoise::tests::chroma_denoise_20mp_bench`. Full-decode
delta lands a bit higher because the Cb / Cr planes and their scratch
buffers compete with the rest of the pipeline for cache.

Inner blur rows (`blur_horizontal_row` and `blur_vertical_row`) are
annotated with `#[multiversion(targets("aarch64+neon",
"x86_64+avx+avx2+fma"))]` and use `f32::mul_add` for FMA hints, same
pattern Phase 6.3 introduced for the lens-correction resampler.

### Pixel-delta summary on real samples

With and without the stage, comparing full `load_image` outputs:

| File          | Mean ¦Δ¦ | Max | Bytes changed |
| ------------- | -------- | --- | ------------- |
| sample1.arw   | 1.06 | 164 | 47 % |
| sample2.dng   | 1.65 | 176 | 60 % |
| sample3.arw   | 2.23 | 183 | 61 % |

High max deltas come from scattered bright-colored noise pixels the blur
absorbs into their neighbors — exactly the intended effect. Visually the
before / after pairs read as "same scene, slightly cleaner flats". Sharp
edges (text, hair, fabric stripes, wall corners) survive intact because
luminance is untouched.

### Settings panel

A new "Denoise" section header sits between "Detail" and "Geometry" with
one toggle row "Chroma noise reduction" / "Mild Gaussian blur on color
channels; keeps luminance sharp." Tag constant `TAG_CHROMA_DENOISE =
160` (the 1xx decade per section pattern the other rows follow). Row
indices in the panel layout + `setCustomSpacing_afterView` calls bumped
from `[11] lens_correction` / `[12] HDR output` to `[12] lens_correction`
/ `[13] HDR output`.

## Phase 6.2 — Clarity (local contrast)

Local-contrast enhancement pass that lifts midtone features — shape
silhouettes, textures — so the image reads crisper at every zoom level,
not just at 100 %. The algorithm is the same separable-Gaussian unsharp
mask `color::sharpen` already uses, applied at a much larger radius
(`σ ≈ 10 px` vs. `0.8 px`). The new module `color::clarity` is a thin
delegator over `color::sharpen::sharpen_*_inplace_with` — if the two
passes ever need to diverge (different kernel, different space), we
extract the shared core; for now the math is genuinely the same, only
the defaults differ.

### Why this closes the "crispness gap" against Affinity

Capture sharpening (σ = 0.8 px) operates on fine pixel-edge detail, so
its contribution is only visible at 100 % zoom; at fit-to-window the
display downsample averages it out. Clarity (σ ≈ 10 px) operates on
midtone features, which survive display downscaling. Lightroom's
"Clarity" slider and Affinity's "Detail Refinement" slider both sit in
this frequency band. Before 6.2, Prvw's output looked slightly "soft" at
fit-to-window vs. Affinity on the same RAW; after 6.2, the two match
visibly on sample1 / sample3 for the default slider positions.

### Defaults

- `DEFAULT_RADIUS = 10.0 px`. Affinity's "Detail Refinement" default
  reads around σ = 20–25 px; we stay more conservative so halos don't
  appear on high-contrast edges.
- `DEFAULT_AMOUNT = 0.40`. Moderate — Affinity's default reads around
  0.5–0.6 by visual inspection; 0.4 gives a pleasant lift without the
  "processed" look.

### Pipeline position

Runs in display-space RGBA8 / RGBA16F **before** capture sharpening, so
the order is clarity (mid-frequency lift) → capture sharpening (fine
edges). Both operate on luminance only; their effects compose cleanly.

### Perf

At σ = 10 the kernel is 61 taps. A 20 MP RGBA8 buffer: 2 separable
passes × 61 × 20 M ≈ 2.4 B FMAs. On Apple Silicon with NEON and rayon
that's typically 200–400 ms — not free, but acceptable for "always on"
default behavior. Users on slow machines can flip the toggle off.

### Settings panel

A new toggle row "Clarity (local contrast)" sits at the top of the
"Detail" section, directly above "Capture sharpening", mirroring the
pipeline order. Two sliders follow: "Clarity radius" (2–50 px, label
format integer-px) and "Clarity amount" (0.00–1.00, two-decimal). Tag
constants `TAG_CLARITY = 130` / `TAG_CLARITY_RADIUS = 204` /
`TAG_CLARITY_AMOUNT = 205`. Row indices in the panel layout bumped by
one from the previous detail/denoise/geometry/output block.

The slider row builder grew a small `LabelFormat` enum
(`TwoDecimal` | `IntegerPx`) so one factory can emit both "0.40" and
"10 px" labels without parallel constructors.
