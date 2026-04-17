# Camera RAW support: Phase 3.0 + 3.1

Phase 2 closed the viewer-polish gap against Preview.app: wide-gamut
intermediate, baseline exposure, tone curve, saturation, and capture
sharpening. Phase 3.0 closes the **DNG correctness** gap. Rawler parses a
handful of DNG tags that its own develop pipeline then ignores. We pick up
those tags and apply them ourselves, per Adobe's DNG spec 1.6 chapter 6.

Phase 3.1 adds **highlight recovery**: a desaturate-to-luminance step that
keeps near-clip pixels from drifting magenta / cyan.

Last updated: 2026-04-17 (Phase 3.1 shipped).

## Scope

- [x] `LinearizationTable` (tag 50712) — verified handled by rawler.
- [x] `OpcodeList1` (tag 51008) — parsed + applied on the raw mosaic
      before linearization. No opcodes on any of our fixtures, but the
      pipeline slot is wired up and tested.
- [x] `OpcodeList2` (tag 51009) — parsed + applied on the rescaled CFA
      mosaic before demosaic. Fires on sample2.dng (iPhone ProRAW): four
      per-Bayer-phase `GainMap`s for lens shading correction.
- [x] `OpcodeList3` (tag 51022) — parsed + applied on the demosaiced,
      linear-Rec.2020 buffer after color conversion. Fires on sample2.dng:
      one `WarpRectilinear` for optical distortion correction.

## LinearizationTable is rawler's job

Rawler's `src/decoders/mod.rs::641` already applies `LinearizationTable`
during raw decoding, via the generic `apply_linearization` helper:

```rust
if let Some(lintable) = ifd.get_entry(TiffCommonTag::Linearization) {
    apply_linearization(&mut pixbuf, &lintable.value, bits);
}
```

`TiffCommonTag::Linearization = 0xC618 = 50712` — the DNG spec's
`LinearizationTable` tag. Rawler builds a `LookupTable` from the values and
dithered-interpolates every raw pixel through it. Our synthetic Bayer DNG
doesn't carry this tag, so the existing golden regression test is
unaffected.

We don't reimplement it. Documented in `dng_opcodes.rs`'s module doc.

## Pipeline after Phase 3.0

Previous pipeline (Phase 2.5b wrap-up, unchanged here):

```
raw_image (rawler) → Rescale + Demosaic + CropActiveArea (rawler)
  → camera_to_linear_rec2020 → apply_default_crop → apply_exposure
  → apply_default_tone_curve → apply_saturation_boost
  → transform_f32_with_profile → rec2020_to_rgba8
  → sharpen_rgba8_inplace → apply_orientation
```

New pipeline:

```
raw_image (rawler)
  → [OpcodeList1]                     ← NEW, on CFA mosaic pre-rescale
  → raw.apply_scaling()               ← moved out of RawDevelop
  → [OpcodeList2]                     ← NEW, on CFA mosaic post-rescale
  → Demosaic + CropActiveArea (rawler)
  → camera_to_linear_rec2020
  → [OpcodeList3]                     ← NEW, on post-color RGB
  → apply_default_crop → apply_exposure
  → apply_default_tone_curve → apply_saturation_boost
  → transform_f32_with_profile → rec2020_to_rgba8
  → sharpen_rgba8_inplace → apply_orientation
```

The four new steps are silent no-ops for non-DNG files and for DNGs that
don't carry the relevant tag. The opcode pipeline slots match DNG spec
§ 6: OpcodeList1 is pre-linearization, OpcodeList2 is post-linearization
pre-demosaic, OpcodeList3 is post-demosaic post-color.

## Opcode status

| ID | Name                   | Parse | Apply | Notes |
|----|------------------------|-------|-------|-------|
| 1  | WarpRectilinear        | ✅    | ✅ (post-color RGB) | Wired into OpcodeList3. Fires on iPhone sample2.dng. |
| 2  | WarpFisheye            | -     | stub  | Log + skip. Not seen on our fixtures. |
| 3  | FixVignetteRadial      | -     | stub  | Log + skip. |
| 4  | FixBadPixelsConstant   | ✅    | ✅ (CFA) | Implemented; no fixture exercises it. |
| 5  | FixBadPixelsList       | ✅    | ✅ (CFA) | Implemented; no fixture exercises it. |
| 6  | TrimBounds             | -     | stub  | Rawler already handles active-area crop. |
| 7  | MapPolynomial          | -     | stub  | Rawler's LinearizationTable path covers Nikon's use. |
| 8  | MapTable               | -     | stub  | Ditto. |
| 9  | GainMap                | ✅    | ✅ (CFA + RGB) | Main workhorse. Fires 4× on iPhone sample2.dng. |
| 10–13 | DeltaPerRow etc.    | -     | stub  | Rare outside MapPolynomial-driven files. |

"Stub" means: we log at debug level if the opcode is flagged optional, at
warn level if it's mandatory, then skip. The decode completes. Better a
best-effort render than a "can't open" dialog.

## iPhone ProRAW specifics

Sample2.dng is an iPhone 13 Pro shot in ProRAW. The relevant opcodes:

- **OpcodeList2**: four `GainMap` opcodes (mandatory). Each grid is 30×40
  f32 points over the full image rect, with `pitch = (2, 2)`, starting at
  `(0, 0)`, `(0, 1)`, `(1, 0)`, `(1, 1)` — one GainMap per Bayer phase.
  Together they implement the per-pixel lens-shading correction Apple's
  demosaic needs.
- **OpcodeList3**: one `WarpRectilinear` opcode (optional). `cx = cy = 0.5`
  (optical center at image center), single-plane parameters — applied
  identically to R, G, B. Plus two `Unknown(14)` opcodes (optional,
  Apple-specific) that we log and skip.

On the Sony ARW and Fujifilm RAF fixtures we tested, no DNG opcode tags
are present — the opcode passes are silent no-ops.

## Unit-test coverage

`decoding::dng_opcodes::tests`:

- `parse_empty_blob` / `parse_zero_count_blob` — degenerate inputs
- `parse_truncated_header` / `parse_truncated_opcode` — malformed inputs
- `parse_two_opcodes_with_flags` — round-trip two opcodes, verify flags
- `parse_unknown_opcode_round_trips_numeric` — unknown IDs preserve their
  numeric value through the round-trip
- `parse_refuses_insane_count` — guard against adversarial counts
- `gain_map_identity_leaves_data_unchanged` — all-ones gain on CFA
- `gain_map_scales_corner_pixels` — uniform non-unity gain on CFA
- `gain_map_is_bayer_aware` — plane=0 on RGGB only modifies R pixels
- `gain_map_on_rgb_only_touches_target_plane` — plane=1 on RGB only
  modifies G channel
- `gain_map_bilinear_interpolates_between_corners` — 2×2 grid, four
  known-output pixels
- `warp_rectilinear_identity_is_noop` — kr0=1 identity warp leaves input
  unchanged (within bilinear rounding)
- `fix_bad_pixels_list_replaces_listed_coord` — center pixel of a flat
  field is repaired from its 8 neighbors
- `fix_bad_pixels_constant_averages_neighbors` — same repair, different
  trigger
- `fix_bad_pixels_list_handles_empty_list` — no-op on empty list

## Ignored smoke tests

Two `#[ignore]` tests under `decoding::raw::tests` exercise the full
pipeline on real RAWs in `/tmp/raw/` when present:

- `dng_opcodes_smoke` — decodes sample2.dng, logs at info level. Expect
  `DNG OpcodeList2: 4 opcode(s)` and `DNG OpcodeList3: 3 opcode(s)`.
- `arw_opcodes_noop_smoke` — decodes sample1.arw and sample3.arw. Expect
  no opcode log lines (ARW has no DNG opcode tags), dimensions match
  Phase 2 expectations.

Run with:

```sh
cd apps/desktop
RUST_LOG=prvw=debug cargo test --release dng_opcodes_smoke \
    -- --ignored --nocapture
```

## `raw-dev-dump` updates

The per-stage dumper example gained three new stages:

- `after-opcode1` — CFA mosaic after OpcodeList1, grayscale preview.
- `after-opcode2` — CFA mosaic after OpcodeList2. On iPhone ProRAW, side-
  by-side with `after-opcode1` you can see the corner lift from the four
  GainMaps.
- `after-opcode3` — linear Rec.2020 after OpcodeList3. On iPhone, shows
  the subtle barrel-distortion correction from `WarpRectilinear`.

A sibling example, `dng-opcodes-inspect`, dumps raw opcode headers for a
DNG — useful for figuring out what a new camera's files actually carry
without running the full decode.

## Performance

End-to-end decode on sample2.dng (M3 Max, release):

- Pre-Phase-3: ~280 ms (extrapolated from Phase 2 numbers)
- Phase 3: `dng_opcodes_smoke` reports a complete decode in ~210–250 ms

The four OpcodeList2 GainMaps each run Rayon-parallel over pixel rows,
adding ~20 ms total on a 12 MP buffer. WarpRectilinear (post-color,
single-pass) is the most expensive at ~30–40 ms on the full 4006×3016
buffer, but we only hit that cost when an opcode fires. Non-DNG files pay
zero overhead.

## Future work

- **CFA opcode coordinate shift**: currently we treat opcode coordinates as
  already matching rawler's raw buffer origin. Cameras with a nonzero
  `active_area` origin would need a shift before applying OpcodeList3.
  None of the fixtures we support hit this path.
- **`MapPolynomial` / `MapTable`** for per-pixel remap. Not urgent — rawler's
  `LinearizationTable` path already covers the Nikon common case.
- **`WarpFisheye`** for ultra-wide lenses. Low priority — no fixture in
  scope.
- **OpcodeList3 WarpRectilinear on sample2.dng looks subtle.** Phase 3.x
  could benchmark against Preview.app's output to confirm the warp formula
  matches Adobe's implementation exactly (there's a normalisation-axis
  choice — image-diagonal vs. longer-side — that different decoders pick
  differently).

## Phase 3.1 — highlight recovery

The pre-3.1 pipeline would pass a clipped channel straight to the tone
curve. A pixel like `(R=1.10, G=0.86, B=0.80)` (bright cloud, R clipped by
exposure lift) hit the tone curve with R still above 1.0, came out as
`(1.0, 0.86→curve, 0.80→curve)`, and landed in RGBA8 at something like
`(255, 205, 195)` — a visibly pink cloud where it should read neutral.
Real RAW renderers handle this by reconstructing the clipped channel from
the unclipped ones.

### Algorithm — desaturate to luminance via smoothstep

For every pixel (R, G, B) in linear Rec.2020:

```text
m = max(R, G, B)
if m <= threshold: no change
else:
  t = smoothstep(threshold, ceiling, m)
  Y = luma_rec2020(R, G, B)
  (R, G, B) = mix((R, G, B), (Y, Y, Y), t)
```

Result: in-gamut pixels pass through bit-identical. Near-clip pixels drift
toward their own luminance, preserving perceived brightness while
surrendering chroma. Above `ceiling`, the pixel is pure gray at luminance
`Y`; the downstream tone curve then compresses that gray to near-white
without any hue shift.

We don't clamp to 1.0: an over-ceiling neutral stays above 1.0 so the tone
curve shoulder still shapes it the way it would shape a normal highlight.

Luma weights are the Rec.2020 coefficients, matching the tone curve and
saturation modules.

### Why desaturate-to-luminance rather than rebuild

dcraw's "rebuild" modes reconstruct clipped channels from the unclipped
ones. That can recover actual color detail, but it risks colored fringe
artifacts at clip boundaries. For a viewer (not an editor), blend-to-
neutral is reliable, has no artifacts, preserves hue direction (no R:G:B
inversion), and matches the natural eye expectation that bright
highlights drift toward white, not magenta.

### Parameter choices

- **`DEFAULT_THRESHOLD = 0.95`**: sits just under the sensor clip point.
  Catches pixels that are about to lose a channel while leaving safely
  in-gamut content alone. Close to dcraw's blend-mode default.
- **`DEFAULT_CEILING = 1.20`**: gives a ~0.25-wide transition region. The
  +0.25 headroom above 1.0 covers the range that a +0.5 EV baseline-
  exposure lift (clamp ±2 EV) can plausibly push a near-clip pixel into.

Both are `pub const`s on the module; the parametric
`apply_highlight_recovery(rgb, threshold, ceiling)` is exposed alongside
`apply_default_highlight_recovery(rgb)` so future per-camera tuning (Phase
3.3 DCP) can override them.

### Pipeline position

Inserted between `apply_exposure` and `apply_default_tone_curve`. Reasons:

- Exposure can push in-gamut values above 1.0 into recovery territory; we
  want the recovery pass to see the post-lift values so we address both
  native sensor clipping and exposure-induced overflow in one step.
- The tone curve has to see a hue-consistent input. If a magenta-shifted
  near-clip pixel survived into the tone curve, no luminance-only curve
  would remove the magenta.

The saturation boost stays where it was (post-tone, pre-ICC). It doesn't
touch recovered pixels much because those are near-neutral by construction
and the boost's `(R - Y)` delta is already small on them.

### Smoke-test observations

On `/tmp/raw/sample1.arw` (silhouettes against bright sky, M3 Max release
build):

- **Pixels changed**: ~304 K out of ~20 M (1.5 %). The rest pass through
  byte-identical, as expected — the rest of the frame is well inside the
  threshold after exposure.
- **Biggest hue rescues** (before → after RGB8, post-pipeline):
  `(255, 205, 195)` → `(229, 224, 223)` — pink cloud to near-neutral
  white. `(194, 249, 255)` → `(226, 240, 246)` — cyan highlight to
  near-white. That's the textbook case: R (or B) clipped, the other two
  channels kept rising, and the pre-3.1 output ended up hue-shifted.
- **Preview comparison**: the `post-exposure.png` preview in `raw-dev-
  dump` looks similar to `post-highlight-recovery.png` because the
  preview's own sRGB clip to `[0, 1]` hides the changes on already-
  clipped pixels. The real win shows up in `final.png`: fewer pink /
  cyan artifacts in the brightest regions.
- **Per-stage runtime**: ~260 ms on a 20 MP buffer (rayon-parallel, one
  traversal). In line with the other linear-domain stages.

### Regression coverage

- `color::highlight_recovery::tests` — 13 pure-function unit tests:
  pass-through, threshold / ceiling endpoints, partial recovery matches
  the formula, monotonic recovery amount `t` across the transition, hue
  direction preserved, neutral inputs unchanged, NaN and negative inputs
  safe, degenerate / inverted parameters fall back to a hard step.
- `decoding::tests::synthetic_dng_matches_golden` — the Phase 2 golden
  regenerated after recovery landed. The synthetic Bayer fixture's
  saturated-magenta gradient desaturates toward neutral above threshold,
  which is the expected behavior.

### Files touched

- `apps/desktop/src/color/highlight_recovery.rs` (new)
- `apps/desktop/src/color/mod.rs` — register module
- `apps/desktop/src/decoding/raw.rs` — wire the step, update module doc
- `apps/desktop/examples/raw-dev-dump.rs` — add `post-highlight-recovery`
  stage
- `apps/desktop/tests/fixtures/raw/synthetic-bayer-128.golden.png` —
  regenerated
- docs: `raw-roadmap.md`, `raw-support-phase3.md`, `CLAUDE.md`s,
  `CHANGELOG.md`
