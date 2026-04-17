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

### Phase 2.1 — Wide-gamut working space (TBD)

Run the develop pipeline into a wide-gamut linear space (Rec.2020 or Wide
Gamut RGB) rather than sRGB, then hand the buffer to the ICC transform to
convert to the display profile. The ICC transform already handles that last
step correctly; the win is not clipping saturated pixels before the
gamut-mapped transform runs.

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
