# Camera RAW support: Phase 3.0 + 3.1 + 3.2 + 3.3 + 3.4 + 3.5

Phase 2 closed the viewer-polish gap against Preview.app: wide-gamut
intermediate, baseline exposure, tone curve, saturation, and capture
sharpening. Phase 3.0 closes the **DNG correctness** gap. Rawler parses a
handful of DNG tags that its own develop pipeline then ignores. We pick up
those tags and apply them ourselves, per Adobe's DNG spec 1.6 chapter 6.

Phase 3.1 adds **highlight recovery**: a desaturate-to-luminance step that
keeps near-clip pixels from drifting magenta / cyan.

Phase 3.2 adds **DCP (Adobe Digital Camera Profile) support**: opt-in per-
camera color refinement that picks up where a generic 3×3 matrix leaves off.

Phase 3.3 extends that to **DNG-embedded profiles**: smartphone DNGs
(Pixel, Galaxy, iPhone ProRAW) and Adobe-converted DNGs carry the same
profile tags directly in their main IFD, and Prvw now honors them
automatically with zero user config.

Phase 3.4 closes the three Phase 3.2-deferred items: DCP **`LookTable`**
(second HSV LUT), **`ProfileToneCurve`** (per-camera tone curve swap), and
**dual-illuminant interpolation** (blend `HueSatMap1` and `HueSatMap2` by
scene color temperature).

Phase 3.5 bundles 161 RawTherapee community DCPs into the binary and adds
**fuzzy camera family matching** so most cameras get per-camera color
fidelity with zero user setup.

Last updated: 2026-04-17 (Phase 3.6 shipped).

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
- `gain_map_cfa_planes_1_touches_every_pixel_regardless_of_bayer` —
  pins the Phase 3.2 hotfix: on CFA photometric, `Plane = 0, Planes = 1`
  applies to every pixel the rect + pitch select, NOT only to pixels of
  a matching CFA color
- `gain_map_cfa_pitch_2_reaches_only_the_matching_bayer_positions` —
  the iPhone ProRAW pattern: spatial pitch `(2, 2)` picks one Bayer
  phase per GainMap entry
- `gain_map_on_rgb_only_touches_target_plane` — plane=1 on RGB only
  modifies G channel
- `gain_map_on_rgb_planes_3_map_planes_1_applies_to_all_channels` —
  Phase 3.6: `Plane = 0, Planes = 3, MapPlanes = 1` fans the single gain
  plane out to R, G, and B
- `gain_map_rgb_map_planes_1_planes_3_applies_uniform_gain_to_all_channels` —
  same as above via the multi-plane builder, uniform 2×
- `gain_map_rgb_map_planes_3_planes_3_applies_per_channel_gain` —
  `MapPlanes = Planes = 3`: distinct gains per channel (2.0 / 3.0 / 4.0)
- `gain_map_rgb_plane_1_planes_1_touches_only_g` — single-plane-single-
  channel path still works (regression guard)
- `gain_map_bilinear_interpolates_between_corners` — 2×2 grid, four
  known-output pixels
- `warp_rectilinear_identity_is_noop` — kr0=1 identity warp leaves input
  unchanged (within bilinear rounding)
- `fix_bad_pixels_list_replaces_listed_coord` — center pixel of a 5×5
  flat field is repaired from its same-phase (step-2) neighbors
- `fix_bad_pixels_constant_averages_neighbors` — same repair, different
  trigger, 5×5 buffer
- `fix_bad_pixels_list_handles_empty_list` — no-op on empty list
- `fix_bad_pixels_uses_same_phase_neighbors_not_adjacent` — Phase 3.6:
  step-1 neighbors at 9.0, step-2 at 1.0; verifies result = 1.0
- `same_phase_neighbor_offsets_returns_eight_step2_pairs` — helper returns
  exactly 8 offsets, all at `{±2}` magnitude, no `(0, 0)`

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

## Phase 3.2 — DCP (Adobe Digital Camera Profile) support

A `.dcp` file captures per-camera color refinement that a generic 3×3
matrix can't express: the distinctive way a Sony A7 III renders skin
tones, how saturated reds roll off on a Canon R5, etc. Applying a matching
DCP is Lightroom's single biggest quality lift over a naïve matrix-only
develop.

Phase 3.2 is **opt-in**: users bring their own DCPs (Adobe's license on
the bundled-with-Lightroom profiles is ambiguous for redistribution).
Without a DCP, Prvw behaves exactly like Phase 3.1.

### What's implemented

- **Parser** (`color::dcp::parser`) for the DCP binary format: a TIFF-like
  container with `IIRC` magic and a standard TIFF IFD. Pulls
  `UniqueCameraModel`, `ProfileName`, `ProfileCopyright`,
  `ProfileCalibrationSignature`, `CalibrationIlluminant1/2`,
  `ProfileHueSatMapDims`, `ProfileHueSatMapData1/2`, and
  `ProfileHueSatMapEncoding`. Refuses malformed input (bad magic,
  out-of-bounds offsets, implausible entry counts) rather than panicking.
- **Apply** (`color::dcp::apply`) converts RGB → HSV, trilinearly
  interpolates the 3D LUT at `(H, S, V)`, applies `hue_shift_degrees`,
  `sat_scale`, and `val_scale` to the pixel's HSV, then converts back.
  Hue axis is cyclic (wraps `360° == 0°`); sat and val axes clamp. Early-
  exit on neutral pixels (`S == 0`) so grays never drift chroma.
- **Discovery** (`color::dcp::discovery`) scans `$PRVW_DCP_DIR` (override)
  then `~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/` and
  its `Adobe Standard/` sibling. Matches by `UniqueCameraModel` (case-
  insensitive, whitespace-insensitive), falling back to
  `ProfileCalibrationSignature`. Returns `None` on no match, which is the
  common case for users without ACR installed.
- **Pipeline integration**: runs post-highlight-recovery,
  pre-tone-curve, in linear Rec.2020. Same slot as Lightroom's "Camera
  Calibration" pane.

### What's deferred

- **LookTable** (`ProfileLookTableData`). Same shape and math as
  HueSatMap, applied as a second pass with its own encoding. Nice-to-have
  for extra quality, but HueSatMap alone captures the bulk of the
  refinement.
- **ProfileToneCurve**. Our default luminance-only tone curve is tuned
  against Preview.app screenshots (Phase 2.5b) and is close to the Adobe-
  neutral curve already. Per-camera swap would change contrast for
  unpredictable reasons.
- **Forward matrix swap** (`ForwardMatrix1/2`). Our pipeline targets
  linear Rec.2020; DCP forward matrices target ProPhoto D50. A proper
  swap would need a full chromatic adaptation re-pipe. Deferred.
- **Dual-illuminant interpolation**. The DNG spec defines a blend between
  `HueSatMap1` and `HueSatMap2` based on the scene's color temperature
  (as estimated from the raw white balance). We always pick the D65
  slot (illuminant 21) straight through, which matches our D65 camera-
  matrix assumption. This leaves accuracy on the table for mixed- or
  tungsten-lit scenes but keeps the code simple and correct at the
  common case.

### HSV conventions used

The DNG spec's normalized HSV is:

- `H ∈ [0, 6)` (six hue sectors; shift values in degrees are divided by
  60 before being added to H).
- `S ∈ [0, 1]` where `S = (max − min) / max`.
- `V = max(R, G, B)`, unbounded above (exposure can push it past 1.0;
  that's fine).

Computed directly on the linear-light RGB values we pass in — no gamma
bake-in. Identity LUT (all hue shifts 0, all sat/val scales 1.0) passes
every pixel through unchanged.

### Matching logic

We compose `"<make> <model>"` from rawler's `Camera` fields and match
it against each DCP's `UniqueCameraModel`. Normalization lowercases and
collapses runs of whitespace, matching the DNG spec's "loose match"
rule. Falls back to `ProfileCalibrationSignature` if `UniqueCameraModel`
doesn't match — rare in practice but occasionally used for "universal"
DCPs.

### Fallback behavior

- **No `PRVW_DCP_DIR`, no ACR install**: `find_dcp_for_camera` scans the
  default paths, finds them missing, returns `None`. The pipeline keeps
  running, output is byte-for-byte identical to Phase 3.1.
- **`PRVW_DCP_DIR` set but directory missing or empty**: same — `None`
  return, no-op.
- **DCP parse error**: logged at debug level, treated as "skip this
  file", continue scanning the rest of the directory.
- **DCP mismatched to camera**: continue scanning.

### Performance

On an M3 Max in release builds:

- DCP parse: ~16 µs warm, once per decode (cached file-system reads).
- DCP apply on a 20 MP buffer (90×30×1 HueSatMap, rayon-parallel):
  ~35 ms. Cheap enough to be imperceptible on modern hardware; the rest
  of the pipeline still dominates.

### Unit-test coverage

`color::dcp::parser::tests`:

- `rejects_too_short`, `rejects_bad_magic`, `rejects_bad_ifd_offset`,
  `rejects_implausible_entry_count` — malformed inputs.
- `parses_minimal_dcp` — round-trip through a synthesized identity DCP.
- `pick_hue_sat_map_prefers_d65` — illuminant-2-is-D65 picks slot 2.
- `pick_hue_sat_map_falls_back_to_single` — no slot 2 → slot 1.
- `huesat_map_sample_at_corners` — index layout matches DNG spec.
- `real_world_dcp_parses` (ignored, requires
  `/tmp/prvw-dcp-test/SONY_ILCE-7M3.dcp`) — round-trips a real Adobe-
  format DCP (from RawTherapee's bundle).
- `parse_bench` (ignored) — perf of parsing a real DCP.

`color::dcp::apply::tests`:

- `rgb_hsv_roundtrip` — RGB → HSV → RGB is identity for well-behaved
  inputs.
- `identity_map_is_pass_through` — LUT of (0°, 1.0, 1.0) is a no-op.
- `neutral_pixels_unchanged_under_identity` — pure grays stay gray.
- `known_hue_shift_rotates_red_toward_yellow` — +60° shift on pure red
  gives pure yellow.
- `known_val_scale_doubles_brightness` — val_scale = 2 doubles RGB.
- `sat_scale_zero_desaturates_to_gray` — sat_scale = 0 collapses color
  to its luminance.
- `hue_wraparound_between_last_and_first_index` — +90° and -90° shifts
  at the LUT's cyclic boundary cancel correctly.
- `nan_pixel_passes_through_untouched` — NaN doesn't crash or smear.
- `val_axis_single_slab_is_stable` — val_divs = 1 (the Adobe 2D case)
  interpolates cleanly.
- `apply_hsm_bench` (ignored) — perf of applying a 90×30×1 LUT to 20 MP.

`color::dcp::discovery::tests`:

- `normalize_collapses_whitespace_and_cases` — normalization rule.
- `dcp_matches_by_unique_camera_model` — match by `UniqueCameraModel`.
- `dcp_matches_by_calibration_signature` — fallback to signature.
- `find_dcp_uses_env_var_path` — `$PRVW_DCP_DIR` discovery round-trip.

### End-to-end smoke test

`decoding::raw::tests::dcp_smoke` (ignored, needs `/tmp/raw/sample1.arw`
and a matching DCP at `/tmp/prvw-dcp-test/`) runs three decodes:

1. `PRVW_DCP_DIR` unset → baseline (Phase 3.1).
2. `PRVW_DCP_DIR` pointing at an empty path → asserts bit-for-bit
   equal to baseline.
3. `PRVW_DCP_DIR` pointing at a dir with a matching DCP → asserts a
   visible shift (> 1 % of bytes changed).

Prints the delta stats so runs document themselves:

```
DCP smoke: 45801361/79264768 bytes changed (57.8%), mean |Δ| = 3.16
```

Set `PRVW_DCP_SMOKE_DUMP=/some/dir` to also emit `baseline.png` and
`with-dcp.png` for visual inspection. Running the smoke with a
relabeled-as-Sony-ILCE-5000 copy of `SONY ILCE-7M3.dcp` on our ARW
fixture:

- Mean per-channel Δ across the whole frame: `R = −0.92, G = +0.49,
  B = −1.66`. Net shift is slightly warmer and slightly less blue —
  consistent with an A7 III-style color profile.
- 92 % of pixels see at least one channel change; max per-pixel Δ is
  102 (on R).
- Spot-check pixel samples show blues shifting bluer and greens
  shifting greener, which is the "per-camera vibrancy" DCP effect.

### Setting up the test DCP

For local testing, we use a RawTherapee-bundled DCP (BSD-licensed) and
rewrite its `UniqueCameraModel` to match our Sony ARW fixture:

```sh
mkdir -p /tmp/prvw-dcp-test
curl -sL -o /tmp/prvw-dcp-test/SONY_ILCE-7M3.dcp \
    'https://raw.githubusercontent.com/Beep6581/RawTherapee/dev/rtdata/dcpprofiles/SONY%20ILCE-7M3.dcp'

# Relabel UniqueCameraModel from "Sony ILCE-7M3\0" (14 bytes) to
# "Sony ILCE-5000" (14 bytes, no null). In-place byte swap at offset
# 0xf2.
python3 -c "
import struct
with open('/tmp/prvw-dcp-test/SONY_ILCE-7M3.dcp', 'rb') as f:
    d = bytearray(f.read())
# UCM entry lives at 0xf2, fixed 14-byte slot. See 'dcp-inspect' output
# for exact offsets.
d[0xf2:0xf2+14] = b'Sony ILCE-5000'
with open('/tmp/prvw-dcp-test/Sony_ILCE-5000-test.dcp', 'wb') as f:
    f.write(d)
"
```

**Don't commit the DCPs.** Adobe's bundled DCPs have ambiguous
redistribution terms; RawTherapee's bundle is BSD but we prefer to keep
the repo clean of camera profiles regardless.

### Inspection tool

`examples/dcp-inspect.rs` dumps a DCP's parsed fields without running the
full decode pipeline. Useful for verifying that a downloaded profile has
the expected tags / dimensions before wiring it up.

```sh
cargo run --example dcp-inspect -- /tmp/prvw-dcp-test/SONY_ILCE-7M3.dcp
```

### Files touched

- `apps/desktop/src/color/dcp/mod.rs` (new): public API + shared test
  helper.
- `apps/desktop/src/color/dcp/parser.rs` (new): binary parser.
- `apps/desktop/src/color/dcp/apply.rs` (new): HSV + trilinear
  interpolation + apply.
- `apps/desktop/src/color/dcp/discovery.rs` (new): filesystem search +
  match.
- `apps/desktop/src/color/mod.rs` — register `dcp` module.
- `apps/desktop/src/decoding/raw.rs` — pipeline insert + smoke test.
- `apps/desktop/examples/dcp-inspect.rs` (new): standalone DCP dumper.
- Docs: `raw-roadmap.md`, this file, `color/CLAUDE.md`,
  `decoding/CLAUDE.md`, `CHANGELOG.md`.

## Phase 3.2 hotfix — CFA GainMap plane-semantics bugfix

Sample2.dng was rendering with a strong red cast that grew with radial
distance from the center — a textbook sign of a lens-shading correction
being applied to one channel only. Root cause: `apply_gain_map_cfa`
filtered each pixel by `cfa_color_at(y, x) == map.plane`, treating the
opcode's `Plane` field as a CFA color index. On iPhone ProRAW, all four
OpcodeList2 `GainMap`s carry `Plane = 0, Planes = 1` and differ only by
their `(Top, Left)` offsets combined with pitch `(2, 2)`. The R-phase
GainMap passed the filter, so R pixels got their corner lift. The
G1/G2/B-phase GainMaps failed the filter and skipped every pixel.
Corners that wanted a ~+30 % uniform gain across all channels got +30 %
on R only, which is the red cast.

DNG spec 1.6 § 6.2.2 (Chapter 7 in the PDF) is clear: "The first plane,
and the number of planes, to be modified are specified by the Plane and
Planes parameters." `Plane` indexes into the photometric interpretation's
image planes, NOT CFA color channels. A CFA photometric image has one
plane — the mosaic itself. Bayer-phase selection comes from `Top/Left`
plus `RowPitch`/`ColPitch`. Apple, LibRaw, RawTherapee, and Adobe's own
SDK all handle it this way.

Fix: `apply_gain_map_cfa` drops the CFA-color filter and its
`cfa_color_at` closure parameter. Every pixel the rect and pitch select
gets the gain. On sample2.dng the red cast is gone and the bathroom reads
neutral end-to-end; on the synthetic DNG and on ARW / CR2 / NEF files
(none of which carry CFA `GainMap`s) the decode is byte-for-byte
identical to Phase 3.2. `apply_gain_map_rgb` is untouched — on a 3-plane
post-demosaic RGB buffer, `Plane` IS the channel index per spec, and the
current one-channel-per-opcode behavior is correct.

Pinned with two new unit tests:
`gain_map_cfa_planes_1_touches_every_pixel_regardless_of_bayer` asserts
every pixel in a rect+pitch 1 rect gets scaled, and
`gain_map_cfa_pitch_2_reaches_only_the_matching_bayer_positions` asserts
the iPhone pattern (one Bayer phase per offset + pitch 2 GainMap).

### Follow-ups (fixed in Phase 3.6)

Both items below were deferred in this commit. They're now closed:

- `apply_gain_map_rgb` now honors `Planes`. When `MapPlanes < Planes`, the
  last gain-map plane fans out to all remaining output planes.
- `FixBadPixelsConstant` / `FixBadPixelsList` now honor `bayer_phase`.
  Both appliers step by 2 in each direction to sample only same-phase
  neighbors.

### Files touched

- `apps/desktop/src/decoding/dng_opcodes.rs` — drop the CFA-color filter
  in `apply_gain_map_cfa`; module doc quotes the spec.
- `apps/desktop/src/decoding/raw.rs` — caller drops the `cfa_color_at`
  closure; no more `cfa` clone.
- `apps/desktop/examples/raw-dev-dump.rs` — mirror the fix.
- Docs: this file, `CHANGELOG.md`.

## Phase 3.6 — DNG GainMap + bad-pixel spec compliance

Both items were flagged as deferred follow-ups in Phase 3.0 (commit
`ecc9973`). No fixture exercises either path; both are spec-correctness
fixes that improve quality on edge cases.

Last updated: 2026-04-17 (Phase 3.6 shipped).

### GainMap `Planes > MapPlanes` fallback

**Problem**: `apply_gain_map_rgb` always modified only the single channel
at `map.plane`, ignoring `map.planes`. A `GainMap` with `Plane = 0,
Planes = 3, MapPlanes = 1` on a post-demosaic buffer was touching only R,
leaving G and B unscaled. Spec § 6.2.2:

> "If Planes > MapPlanes, the last gain map plane is used for any remaining
>  planes being modified."

**Fix**: `apply_gain_map_rgb` now iterates output channels from `first_out`
(`map.plane`) through `last_out` (`map.plane + map.planes`, clamped to 3).
For each output channel, the gain-map plane index is
`min(out_ch_offset, map.map_planes - 1)`, matching the spec's "last plane
fans out" rule. This unifies the RGB path's semantics with the CFA path's
(which was correct after the Phase 3.0 hotfix).

**Sample2.dng impact**: none. Sample2's four OpcodeList2 GainMaps all have
`Planes = 1, MapPlanes = 1` and run through `apply_gain_map_cfa` (CFA
path), not the RGB path. Output is byte-for-byte identical.

**Unit tests added**:

- `gain_map_rgb_map_planes_1_planes_3_applies_uniform_gain_to_all_channels`:
  `MapPlanes = 1, Planes = 3` → same 2× gain on R, G, and B.
- `gain_map_rgb_map_planes_3_planes_3_applies_per_channel_gain`:
  `MapPlanes = 3, Planes = 3` → gains 2.0 / 3.0 / 4.0 applied to R / G / B
  independently. Also verifies per-channel selection still works.
- Updated `gain_map_on_rgb_planes_3_map_planes_1_applies_to_all_channels`
  (was `gain_map_on_rgb_with_plane_0_leaves_g_and_b_untouched`): the old
  test documented the incorrect behavior; the new name and assertion
  document the correct one.

### Bad-pixel `bayer_phase` neighbor selection

**Problem**: `apply_fix_bad_pixels_constant` and `apply_fix_bad_pixels_list`
sampled from all 8 immediate neighbors (step 1 in each direction) regardless
of Bayer phase. For a red bad pixel, this included green and blue neighbors
at step-1 offsets — mixing CFA colors. DNG spec § 6.2.2 implies that
interpolation should stay within the same Bayer phase (only same-color pixels
at step 2).

**Fix**: A new helper `same_phase_neighbor_offsets(y, x, bayer_phase)`
returns the eight `{-2, 0, +2}² \ {(0,0)}` offset pairs. Both appliers
now call this helper instead of the unrestricted `{-1..=1}²` loop. Because
every CFA color repeats every 2 rows and 2 columns, step-2 neighbors are
guaranteed to share the bad pixel's Bayer phase.

The `_y`, `_x`, and `_bayer_phase` parameters are accepted for API
completeness; the step-2 rule is already phase-correct for all four Bayer
positions, so no per-phase branching is needed today.

**Unit tests added**:

- `same_phase_neighbor_offsets_returns_eight_step2_pairs`: verifies the
  helper returns exactly 8 pairs, all at `{±2}` magnitude, none at `(0,0)`.
- `fix_bad_pixels_uses_same_phase_neighbors_not_adjacent`: in a 5×5 CFA
  buffer, step-1 (adjacent) neighbors are set to 9.0 and step-2 (same-phase)
  neighbors to 1.0. Verifies the repaired pixel averages to 1.0 (step-2),
  not 9.0 (step-1).
- Updated `fix_bad_pixels_list_replaces_listed_coord` and
  `fix_bad_pixels_constant_averages_neighbors` to use a 5×5 buffer (center
  pixel at position `(2, 2)`) so step-2 neighbors are in bounds.

### Files touched

- `apps/desktop/src/decoding/dng_opcodes.rs` — `apply_gain_map_rgb`,
  `apply_fix_bad_pixels_constant`, `apply_fix_bad_pixels_list`, new
  `same_phase_neighbor_offsets` helper, struct field docs, module doc,
  six updated or new unit tests.

## Phase 3.3 — apply DCP data embedded in DNG files

Phase 3.2 built an opt-in DCP stack: parse standalone `.dcp` files, match
by `UniqueCameraModel`, apply `ProfileHueSatMap`. That unlocked per-camera
color for any camera the user had a DCP for, but in practice most users
don't install Adobe Camera Raw and have no `.dcp` lying around. Phase 3.3
closes the biggest remaining gap in per-camera color: **DNG files that
embed their own profile**.

Smartphone DNGs are the prime case. A Pixel 6 Pro DNG carries a
`ProfileName` of `"Google Embedded Camera Profile"` sitting right in the
main IFD alongside `ProfileHueSatMapDims`, `ProfileHueSatMapData1`, and
friends. Samsung Galaxy and iPhone ProRAW do the same. Adobe DNG
Converter also bakes a profile into the DNG when you convert an ARW / CR3
/ RAF through it, so anything that's run through the converter ships a
matching profile too. Before Phase 3.3 we ignored every one of those.

### What changed

- **New module** `color::dcp::embedded` with a single public entry
  point: `from_dng_tags(tags: &HashMap<u16, Value>) -> Option<Dcp>`.
  Reads the same DNG 1.6 § 6.2 profile tags the standalone parser knows
  about (`ProfileHueSatMapDims`, `ProfileHueSatMapData1/2`,
  `ProfileHueSatMapEncoding`, `UniqueCameraModel`, `ProfileName`,
  `ProfileCopyright`, `ProfileCalibrationSignature`, and both
  `CalibrationIlluminant` tags) and produces the same `Dcp` struct
  the standalone parser produces, so downstream `apply_hue_sat_map`
  doesn't care where the data came from.
- **New helper** `decoding::raw::collect_dng_profile_tags`. Builds the
  input `HashMap` from (1) `raw.dng_tags` (rawler's RAF decoder
  populates this), (2) `decoder.ifd(WellKnownIFD::VirtualDngRootTags)`,
  (3) `decoder.ifd(WellKnownIFD::Root)`. Returns `None` when no
  relevant tag is present so the DCP code can skip the embedded path
  without a useless allocation.
- **Updated entry point**: `color::dcp::apply_if_available` now takes
  the optional tag map and a camera id, tries the embedded path first,
  then falls back to `find_dcp_for_camera`. Returns `(Dcp, DcpSource)`
  where `DcpSource` is `Embedded` or `Filesystem`.
- **INFO-level log line** spells the source out:
  `"RAW applied EMBEDDED DCP 'Google Embedded Camera Profile' for
  camera 'Google Pixel 6 Pro' on …"` vs. `"RAW applied filesystem DCP
  'SONY ILCE-7M3' for camera 'Sony ILCE-7M3' on …"`.

### Precedence rule

Embedded wins. When a DNG has both an embedded profile and a matching
filesystem DCP, we use the embedded one. The manufacturer's profile is
the authoritative description of how the camera sees color;
overriding it with a third-party file is almost always wrong.

Users who want to override can set `PRVW_DISABLE_EMBEDDED_DCP=1`. That
forces `apply_if_available` to skip the embedded path and fall through
to filesystem discovery. It's there for the smoke test and the rare
expert override — normal operation never needs it.

### Pipeline position

Unchanged from Phase 3.2: the DCP runs **post-highlight-recovery,
pre-tone-curve** in linear Rec.2020. We only added the new discovery
branch; the applier is byte-for-byte identical.

### Smoke-test observations

`decoding::raw::tests::embedded_dcp_smoke` (ignored, needs
`/tmp/raw/sample2.dng`) decodes the Pixel 6 Pro sample twice:
once with `PRVW_DISABLE_EMBEDDED_DCP=1`, once without. Output:

```
RAW applied EMBEDDED DCP 'Google Embedded Camera Profile' for camera
    'Google Pixel 6 Pro' on /tmp/raw/sample2.dng
Embedded DCP smoke: 30296917/47880000 bytes changed (63.3%),
    mean |Δ| = 3.28
```

Visually, the two outputs are clearly different: the without-embedded
version has a slight cool / greenish cast on the tiles and walls that
the Pixel's matrix + our default pipeline leaves behind, while the
with-embedded version renders the bathroom with more balanced, warmer
grays — the neutral look Google's profile designers intended.

Set `PRVW_EMBEDDED_DCP_SMOKE_DUMP=/some/dir` to emit
`without-embedded.png` and `with-embedded.png` for side-by-side
inspection.

### Regression

- **Sony ARW** (`sample1.arw`, `sample3.arw`): no embedded profile tags
  present, `from_dng_tags` returns `None`, the filesystem path runs
  unchanged. The existing `dcp_smoke` test asserts bit-for-bit equality
  with the pre-3.3 baseline in the no-match case, and it still passes.
- **Synthetic Bayer DNG** (`synthetic-bayer-128.dng`): no profile tags
  embedded, `from_dng_tags` returns `None`, no apply. The golden-image
  regression test passes unchanged — no golden regeneration needed.

### Why we parse tags directly instead of piping through the standalone
DCP parser

The standalone `parser::parse` expects the DCP file header magic (`IIRC`
plus an IFD offset) and iterates a private IFD. A DNG's main IFD doesn't
carry that magic and its entries are mixed with thousands of non-profile
tags. Routing through `parse` would mean synthesising a fake DCP file in
memory, which would duplicate rawler's TIFF writer logic for no reason.
`from_dng_tags` is much simpler: it already has the decoded `Value`s in
its hand; it just picks the ones the spec calls out.

### Tests

- **Unit**: seven in `color::dcp::embedded::tests` cover: minimal happy
  path, missing dims returns `None`, missing both data maps returns
  `None`, size mismatch between dims and data, `Data2`-only (no
  `Data1`), full metadata round-trip (name, copyright, illuminants,
  encoding), `Double` fallback when a writer used `f64` instead of
  `f32`.
- **Unit** (existing): `parser::tests::pick_hue_sat_map_falls_back_to_
  single` covers "illuminant tag missing → gracefully pick map 1". The
  embedded path produces the same `Dcp` struct, so that test also
  validates the no-illuminant behaviour end-to-end.
- **Integration** (ignored): `decoding::raw::tests::embedded_dcp_smoke`
  — see above.

### Files touched

- `apps/desktop/src/color/dcp/embedded.rs` (new) — `from_dng_tags`.
- `apps/desktop/src/color/dcp/mod.rs` — re-export `from_dng_tags`,
  refactor `apply_if_available` to take an optional tag map, add the
  `DcpSource` enum, add the `PRVW_DISABLE_EMBEDDED_DCP` override.
- `apps/desktop/src/decoding/raw.rs` — new `collect_dng_profile_tags`
  helper; call the updated `apply_if_available`; INFO log line names
  the source.
- `apps/desktop/src/color/dcp/CLAUDE.md` (new) — document the two
  discovery paths and the precedence rule.
- `apps/desktop/src/color/CLAUDE.md` — update the DCP row to mention
  Phase 3.3.
- Docs: this file, `raw-roadmap.md`, `CHANGELOG.md`.

## Phase 3.4 — DCP LookTable + tone curve + dual-illuminant

Three deferred items from Phase 3.2 land together. All three share the
same plumbing: tags read from either `.dcp` files or DNG IFDs, the same
`Dcp` / `HueSatMap` types downstream, no new pipeline stage — just
additional passes wired into the existing post-highlight-recovery /
pre-tone-curve slot plus a conditional swap of the tone-curve stage
itself.

### LookTable

**Spec**: DNG 1.6 § 6.2.3. Tag IDs 50981 (`ProfileLookTableDims`),
50982 (`ProfileLookTableData`), 51108 (`ProfileLookTableEncoding`).
Same shape and application math as `HueSatMap`: a 3D LUT indexed by
`(hue, sat, val)`, each entry `(hue_shift_deg, sat_scale, val_scale)`,
applied trilinearly in HSV. Single-illuminant — just one payload, no
per-illuminant slots. Applied **after** `HueSatMap` per spec; logically,
`HueSatMap` is the "neutral calibration" and `LookTable` is the Adobe
"Look" that stacks on top.

**Parsing**: extended `color::dcp::parser` and
`color::dcp::embedded::from_dng_tags` to read the three new tags.
Dims / data / encoding wired into the `Dcp` struct as optional
`look_table: Option<HueSatMap>` + `look_table_encoding: u32`.

**Application**: called straight through `apply_hue_sat_map`. The
existing LUT code is agnostic to which tag fed it, so LookTable adds
zero new math.

**Fixture**: on sample2.dng the Pixel 6 Pro's embedded profile ships
a LookTable. On the Sony ILCE-7M3 filesystem DCP too.

### ProfileToneCurve

**Spec**: DNG 1.6 § 6.2.4. Tag 50940. A flat float list interpreted as
`(x, y)` pairs, monotonically increasing, with endpoints at `(0, 0)` and
`(1, 1)`.

**Application choice**: option A (cleaner). A new
`color::tone_curve::apply_tone_curve_lut(rgb, points)` applies the curve
in the **same luminance-only + uniform-RGB-scale** pattern
`apply_default_tone_curve` already uses. The curve is sampled via
piecewise-linear interpolation at the pixel's Rec.2020 luminance, then
RGB is rescaled by `Y_out / Y_in`. Hue and chroma stay preserved,
matching the invariants the rest of the pipeline assumes.

**Policy**: when the active DCP (embedded or filesystem) carries a
ProfileToneCurve, apply it **instead of** our default Hermite S-curve.
The camera's intended tonality is more authoritative than our generic
default; that's the whole reason a profile ships a tone curve to begin
with. Logged at INFO ("used DCP tone curve" vs. "used default tone
curve") so users can tell which curve rendered an image. No DCP →
default runs unchanged.

**Fixture findings**: sample2.dng's embedded profile ships a 257-point
curve; the SONY ILCE-7M3 DCP ships an 8192-point curve. Both get picked
up by the new path.

**Sample-level delta**: mean |Δ| per byte on sample2.dng went from 3.28
(Phase 3.3 — HueSatMap only) to 17.19 (Phase 3.4 — HueSatMap +
LookTable + ProfileToneCurve). The tone curve dominates the increase,
which is expected: our default curve was tuned against Preview.app and
the Pixel's intended curve is visibly different (stronger shadow lift,
more roll-off at the highlight shoulder, warmer midtones). Visual
inspection on the bathroom shot confirms the Pixel-curve rendering
reads closer to Google Photos' own display of the same DNG than the
default curve did.

### Dual-illuminant interpolation

**Spec**: DNG 1.6 § 6.2.5. When a profile has `HueSatMap1` (at
`CalibrationIlluminant1`) and `HueSatMap2` (at `CalibrationIlluminant2`),
blend them based on the scene's correlated color temperature. The full
procedure iterates `ForwardMatrix1/2` + `AsShotNeutral` until the
temperature converges (~3 iterations typical).

**Fidelity choice**: **compromise** (per the brief's recommendation).
Smooth blend, simple temperature estimate. Specifically:

1. `estimate_scene_temp_k(wb_coeffs)` uses the one-shot formula
   `temp ≈ 7000 − 2000 × (R/G − 1)`, clamped to `[2000, 10000] K`.
   Derived from the observation that camera WB coefficients neutralise
   the scene: high R gain → warm scene, high B gain → cool scene.
   Not the spec's procedure, but produces smooth results without the
   discontinuity a discrete switch would create at the boundary.
2. `illuminant_temp_k(code)` maps the DNG spec's `CalibrationIlluminant`
   EXIF codes to Kelvin (17 = A ~ 2856 K, 21 = D65 ~ 6504 K, 23 = D50,
   etc.). Unknown codes fall back to 5000 K.
3. `interpolate_hue_sat_maps(dcp, scene_k)` computes a blend weight
   `t = clamp((scene_k − low_k) / (high_k − low_k), 0, 1)` and
   interpolates the two maps entry-by-entry. `low_k` and `high_k` get
   sorted so `t` stays in `[0, 1]` regardless of which slot is which.
   Single-map DCPs short-circuit to a clone. Shape-mismatched maps fall
   back to the D65-preferring `Dcp::pick_hue_sat_map`.

**Pipeline position**: merge produces a single `HueSatMap` that
replaces what `pick_hue_sat_map` used to return. The rest of the
pipeline (apply, LookTable, tone curve) runs unchanged.

**Limitations we accept**: the WB-to-temp estimate can be off by
several hundred K vs. the spec's iterative procedure on images with
an unusual color-of-subject (heavy foliage, large skin area, etc.).
Good enough for a viewer; color scientists reaching for Prvw can
force a specific illuminant by editing the DCP or can wait for the
full iterative procedure in a later refinement.

### Precedence

When both DCP-embedded and filesystem `.dcp` are available, embedded
still wins (the Phase 3.3 decision). Whichever source wins, the WHOLE
profile comes from that source — `HueSatMap`, `LookTable`, and
`ProfileToneCurve` are never mixed across sources. That's an
invariant: a filesystem DCP's tone curve stacking onto an embedded
DCP's HueSatMap would produce nonsense because the profile author
tuned those two pieces together.

### New INFO log lines

On a successful DCP apply, the decoder now logs both the source and
which optional pieces fired:

```
RAW applied EMBEDDED DCP 'Google Embedded Camera Profile' for camera
    'Google Pixel 6 Pro' on /tmp/raw/sample2.dng [with LookTable]
    [with ToneCurve]
RAW used DCP tone curve (257 points) for /tmp/raw/sample2.dng
```

For filesystem DCPs:

```
RAW applied filesystem DCP 'SONY ILCE-7M3' for camera 'SONY ILCE-5000'
    on /tmp/raw/sample1.arw [with LookTable] [with ToneCurve]
RAW used DCP tone curve (8192 points) for /tmp/raw/sample1.arw
```

### Unit-test coverage

`color::dcp::parser::tests`:
- `parses_dcp_with_look_table_and_tone_curve` — round-trips a synthesised
  DCP carrying all three tag groups.

`color::dcp::embedded::tests`:
- `reads_optional_look_table` — happy-path LookTable from DNG IFD.
- `returns_none_for_look_table_when_dims_or_data_missing` — graceful skip.
- `reads_optional_tone_curve` — happy-path 3-point curve round-trip.
- `tone_curve_rejects_odd_length` — parity check.
- `tone_curve_accepts_double_payload` — f64 fallback.

`color::dcp::apply::tests`:
- `look_table_pass_after_hue_sat_map_darkens_target_band` — HueSatMap
  no-op followed by LookTable that halves red-band brightness. Red
  pixels halve, blue pixels untouched.

`color::tone_curve::tests`:
- `piecewise_linear_interpolates_between_points` — identity curve.
- `piecewise_linear_clamps_outside_domain` — out-of-range clamp.
- `piecewise_linear_three_point_s_curve` — known interior values.
- `apply_tone_curve_lut_identity_is_noop` — identity curve through the
  full buffer apply.
- `apply_tone_curve_lut_preserves_hue` — R:G / R:B ratios invariant.
- `apply_tone_curve_lut_darker_curve_darkens_output` — darker S-curve
  darkens to the expected value.
- `apply_tone_curve_lut_empty_is_noop` — zero-point safety.
- `apply_tone_curve_lut_handles_dark_and_nan` — dark-pixel / NaN safety
  matches the default curve's behaviour.

`color::dcp::illuminant::tests`:
- `scene_temp_warm_for_high_r_over_g` — tungsten-ish WB → warm temp.
- `scene_temp_cool_for_low_r_over_g` — shade-ish WB → cool temp.
- `scene_temp_neutral_for_equal_r_g` — neutral WB → 7000 K midpoint.
- `scene_temp_falls_back_on_bad_coeffs` — NaN / zero safety.
- `scene_temp_clamps_extremes` — pathological WB → clamped range.
- `illuminant_temp_lookup` — spot-checks A, D65, D50.
- `interpolate_single_map_returns_clone` — single-map degenerate case.
- `interpolate_at_low_endpoint_matches_low_map` — scene at warm
  illuminant temp → pure low-K map.
- `interpolate_at_high_endpoint_matches_high_map` — scene at cool
  illuminant temp → pure high-K map.
- `interpolate_at_midpoint_averages` — scene at midpoint → 50/50 blend.
- `interpolate_clamps_outside_range` — scene way outside endpoints →
  clamp to nearest.
- `interpolate_shape_mismatch_falls_back_to_pick` — graceful fallback.

### Smoke-test observations

`decoding::raw::tests::embedded_dcp_smoke` on sample2.dng:

```
RAW applied EMBEDDED DCP 'Google Embedded Camera Profile' for camera
    'Google Pixel 6 Pro' on /tmp/raw/sample2.dng [with LookTable]
    [with ToneCurve]
RAW used DCP tone curve (257 points) for /tmp/raw/sample2.dng
Embedded DCP smoke: 35274843/47880000 bytes changed (73.7%),
    mean |Δ| = 17.19
```

`decoding::raw::tests::dcp_smoke` on sample1.arw + Sony ILCE-7M3 DCP:

```
RAW applied filesystem DCP 'SONY ILCE-7M3' for camera 'SONY ILCE-5000'
    on /tmp/raw/sample1.arw [with LookTable] [with ToneCurve]
RAW used DCP tone curve (8192 points) for /tmp/raw/sample1.arw
DCP smoke: 52380003/79264768 bytes changed (66.1%), mean |Δ| = 14.58
```

Both smoke tests confirm the full stack fires. The mean |Δ| increase
over Phase 3.3 is expected: ProfileToneCurve is typically a big part
of a DCP's visual character, and our default curve (while close to
Adobe's neutral default) is not identical to the per-camera curves the
profile authors ship. The Sony curve lifts shadows harder than our
default; the Pixel curve is punchier in the midtones.

### Regression

- **Synthetic Bayer DNG** (`synthetic-bayer-128.dng`): carries no
  profile tags → DCP path short-circuits on `from_dng_tags` returning
  `None`. The golden-image test passes unchanged.
- **ARW / CR2 / NEF / RAF without a filesystem DCP match**: same
  behaviour — `apply_if_available` returns `None`, default tone curve
  runs, byte-for-byte identical to Phase 3.3.
- **ARW with a filesystem DCP match but no LookTable / ToneCurve**: the
  Phase 3.2 behaviour. Only HueSatMap applies, default tone curve
  runs. (None of our bundled test DCPs hit this path cleanly — all
  three RawTherapee profiles we use carry all three features — but
  the unit tests cover the missing-tag branches.)

### Files touched

- `apps/desktop/src/color/dcp/parser.rs` — LookTable + ToneCurve tag
  IDs, `Dcp` fields, parse loop extensions, tests.
- `apps/desktop/src/color/dcp/embedded.rs` — same tag IDs + struct
  fields + readers + tests for the embedded path.
- `apps/desktop/src/color/dcp/apply.rs` — unit test for the
  LookTable-after-HueSatMap sequence. The applier itself is reused
  unchanged.
- `apps/desktop/src/color/dcp/illuminant.rs` (new) — scene-temp
  estimate, illuminant code table, `interpolate_hue_sat_maps` blend,
  tests.
- `apps/desktop/src/color/dcp/mod.rs` — `apply_if_available` signature
  extended to accept `wb_coeffs`, calls the new blend + LookTable
  apply, returns the DCP so the caller can read its tone curve.
- `apps/desktop/src/color/dcp/discovery.rs` — `Dcp` field additions in
  test fixtures.
- `apps/desktop/src/color/tone_curve.rs` — new
  `apply_tone_curve_lut` + `sample_piecewise_linear` + tests.
- `apps/desktop/src/decoding/raw.rs` — collect the new tags,
  thread `wb_coeffs` into the DCP call, swap the tone-curve stage when
  the DCP carries one, log which curve ran.
- Docs: `raw-roadmap.md`, this file, `color/CLAUDE.md`, `color/dcp/CLAUDE.md`.

## Phase 3.5 — bundled DCP collection + fuzzy family matching

Phase 3.2 made DCP matching opt-in: users with Adobe Camera Raw or a custom
`PRVW_DCP_DIR` got per-camera color. Everyone else got the default pipeline.
Phase 3.5 closes that gap: 161 RawTherapee community profiles are bundled
into the binary, and a fuzzy alias table catches cameras whose exact model
isn't in the collection.

### Bundled collection

**Source**: [RawTherapee `dev` branch, `rtdata/dcpprofiles/`](https://github.com/Beep6581/RawTherapee/tree/dev/rtdata/dcpprofiles).
161 DCP files, community-contributed by Maciej Dworak, Lawrence Lee, Alberto
Griggio, Thanatomanic, Morgan Hardwood, and others. Source RAW files used to
generate the profiles were released under CC0 by their respective photographers.
Attribution in `apps/desktop/build-assets/dcps/LICENSE`.

**Bundle strategy — Strategy B (compressed blob)**:

`build.rs` reads all `.dcp` files from `apps/desktop/build-assets/dcps/`,
concatenates them in sorted order, and compresses the result with zstd at
level 10. Output: `OUT_DIR/bundled_dcps.zst` + `OUT_DIR/bundled_dcps.idx`
(plain-text offset index). Both are `include_bytes!`'d into the binary by
`color::dcp::bundled`.

Why zstd over gzip: DCP float arrays barely compress file-by-file (gzip best:
~81 % of original). Concatenating all 161 DCPs before compressing lets zstd
exploit cross-file repetition in the float data (same HSV grid structures
appear across cameras from the same manufacturer). Result: ~11 MB binary delta
vs. ~67 MB with gzip or ~83 MB uncompressed.

**Binary size delta**: 26.5 MB → 34.9 MB (+9.7 MB for 161 DCPs).

**Runtime**: the blob decompresses once on first DCP lookup (a `OnceLock`),
then stays in memory for the process lifetime (~83 MB heap). Parse is
always done by the shared `parser::parse`; no separate code path.

**Search tier order** (updated from Phase 3.2):
1. Embedded (DNG's own IFD)
2. Filesystem exact (`PRVW_DCP_DIR` + Adobe dir)
3. Bundled exact (new)
4. Fuzzy aliases, each trying filesystem then bundled (new)
5. None — fall back to default pipeline

### Fuzzy family matching

`color::dcp::family_aliases::FAMILY_ALIASES` is a conservative curated table
of `(camera, &[aliases])` pairs. Each alias represents a known-compatible
camera — same sensor chip or close product family — that the caller can
substitute for an exact match. Current seed: 20 entries across Sony, Fujifilm,
Nikon, Canon, Olympus, and Panasonic.

**Matching policy**: conservative is correct. It's better to fall through to
the default matrix pipeline than to apply a Canon profile to a Nikon body.
Add entries via PR with evidence (same sensor chip, DxOMark comparison, or
side-by-side color analysis). Don't add based on brand or marketing alone.

**Log output**: when a fuzzy match fires, an INFO line names the substitution:

```
DCP: no exact match for 'Sony ILCE-5000'; using compatible profile
    'SONY ILCE-6000' from bundled collection
```

The `BundledAlias` and `FilesystemAlias` variants of `DcpSource` carry this
distinction all the way to the INFO log in `decoding::raw`.

### Unit-test coverage

`color::dcp::bundled::tests`:
- `bundled_count_is_nonzero` — build packed at least one DCP.
- `bundled_count_matches_build_assets` — at least 100 DCPs (sanity).
- `find_bundled_dcp_returns_known_camera` — `SONY ILCE-7M3` is in the
  collection; returned DCP has the expected `UniqueCameraModel`.
- `find_bundled_dcp_returns_none_for_unknown_camera` — made-up model
  returns `None`.

`color::dcp::family_aliases::tests`:
- `aliases_for_known_camera_returns_nonempty` — `Sony ILCE-5000` resolves
  to a non-empty alias list including `Sony ILCE-6000`.
- `aliases_for_unknown_camera_returns_empty` — unknown model → empty slice.
- `aliases_for_is_case_and_space_insensitive` — normalization consistent
  with the primary path.
- `all_alias_entries_have_at_least_one_alias` — table integrity check.

### Reproducibility

`scripts/sync-bundled-dcps.sh` re-downloads the full collection from RT's
`dev` branch. Run it to update the bundle when RT ships new profiles. The
script is idempotent (skips files already present) and prints a summary.

### Fuzzy-alias matches skip the whole DCP color stage

Follow-up (2026-04-17): a user reported that a bundled ILCE-6000 profile
applied to an ILCE-5000 (via `FAMILY_ALIASES`) on sample3.arw produced
"unrealistically vibrant colors" and a "comically purple" mouth. Same
class of problem the tone-curve auto-skip caught earlier: same-family
bodies can share a sensor family label but still have different CFA
spectral responses, and a HueSatMap calibrated on one body pushes reds
/ magentas / skin tones in the wrong direction on another.

Fix: `apply_if_available` now takes an `allow_fuzzy: bool` parameter.
The RAW pipeline passes `false`, which makes the function log an INFO
line and return `None` when the only hit came from a `FAMILY_ALIASES`
substitution. Both `HueSatMap` and `LookTable` are skipped; the default
tone curve runs downstream because `dcp_info` is `None`. Exact matches
(embedded, filesystem, bundled) are unaffected. Users who want the
fuzzy profile applied anyway can drop an exact-match DCP under
`$PRVW_DCP_DIR`.

INFO log line: `DCP '<name>' found via fuzzy alias but NOT applied
(avoids cross-sensor color artifacts). Set an exact-match DCP for this
camera to override.`

Unit test: `color::dcp::apply_tests::fuzzy_alias_with_allow_fuzzy_false
_returns_none_and_preserves_buffer` — feeds the fuzzy camera ID
`"Sony ILCE-5000"` through `apply_if_available(..., false)` and asserts
the buffer is byte-identical afterward.

### Files touched

- `apps/desktop/build.rs` (new) — concatenates + zstd-compresses DCPs at
  build time.
- `apps/desktop/build-assets/dcps/` (new dir) — 161 `.dcp` files + LICENSE.
- `apps/desktop/src/color/dcp/bundled.rs` (new) — runtime loader.
- `apps/desktop/src/color/dcp/family_aliases.rs` (new) — alias table.
- `apps/desktop/src/color/dcp/mod.rs` — new tiers in `apply_if_available`,
  `try_aliases` helper, `DcpSource::{Bundled, BundledAlias, FilesystemAlias}`
  variants, updated module doc.
- `apps/desktop/src/color/dcp/discovery.rs` — `normalize` made `pub(super)`.
- `apps/desktop/Cargo.toml` — `zstd = "0.13.3"` added (build + runtime).
- `apps/desktop/src/color/dcp/CLAUDE.md` — updated search-order table.
- `apps/desktop/src/color/CLAUDE.md` — updated DCP row.
- `docs/notes/` — this file + `raw-roadmap.md` updated.
- `CHANGELOG.md` — added Phase 3.5 bullets.

## Phase 3.7 — Settings UI for pipeline transparency

Ships a new **RAW** section in the Settings window with per-stage toggles
plus a custom DCP directory picker. Aim is pre-v1 transparency: users can
see (and flip off) each step the RAW pipeline performs, and diagnose
issues without going through the CLI or building from source.

### Toggles

`RawPipelineFlags` (in `decoding::raw_flags`) carries one bool per stage,
grouped in the UI by pipeline family:

- **Sensor corrections** (DNG only, silent no-op on ARW, CR2, NEF, etc.):
  `dng_opcode_list_1`, `dng_opcode_list_2`, `dng_opcode_list_3`.
- **Color**: `baseline_exposure`, `dcp_hue_sat_map`, `dcp_look_table`,
  `saturation_boost`.
- **Tone**: `highlight_recovery`, `tone_curve` (gates both the default
  Hermite S-curve and any DCP-supplied `ProfileToneCurve`).
- **Detail**: `capture_sharpening`.

All default to `true`. When every flag is at its default, `raw::decode`
produces the exact same bytes as before this phase — the flags wrap each
stage in an `if` at the same position.

### Cache flush + re-decode

Toggling any switch sends `AppCommand::SetRawPipelineFlags(new_flags)`.
`App::execute_command` writes the new flags through to `App.raw_flags`,
saves `settings.json`, swaps them into the preloader, flushes
`ImageCache`, and re-decodes the current image. The flush path lives in
`App::apply_raw_flag_change`, sibling to `flush_and_redisplay` (the same
pattern rendering-intent toggles use).

### One-line diagnostic log

`raw::decode` emits a single INFO line per decode when `flags.is_default()`
returns false:

```
RAW pipeline: 2 step(s) disabled (highlight recovery, capture sharpening) for /tmp/raw/sample3.arw
```

Default path stays silent.

### Custom DCP directory

The same panel hosts a "Custom DCP directory" row. The value lives in
`Settings.custom_dcp_dir` (serialized as `Option<String>`). When the user
picks or clears a path, `AppCommand::SetCustomDcpDir` flows through
`App::apply_custom_dcp_dir_change`, which:

1. Pushes the path into the `PRVW_DCP_DIR` env var (the same knob the
   DCP discovery module already honors — no change needed there).
2. Flushes the image cache.
3. Re-decodes.

Choosing `None` clears the env var so discovery falls back to Adobe
Camera Raw's install dir and the bundled collection.

### Why env var rather than threading a path

`color::dcp::discovery::find_dcp_for_camera` already reads `PRVW_DCP_DIR`
directly. Threading a `custom_dir: Option<&Path>` through the pipeline
would have duplicated the discovery plumbing and added a parameter to
`apply_if_available` for no runtime gain. The env var is process-wide,
which fits: DCP discovery is a process-wide concern and there's no
per-image or per-thread variation. `unsafe` around `std::env::set_var` is
isolated to `app::apply_custom_dcp_dir` and called only from the main
thread (App startup + command executor).

### Files touched

- `apps/desktop/src/decoding/raw_flags.rs` (new) — `RawPipelineFlags`
  struct + unit tests (default, round-trip, disabled-labels order).
- `apps/desktop/src/decoding/mod.rs` — `load_image{,_cancellable}` take
  `raw_flags: RawPipelineFlags`.
- `apps/desktop/src/decoding/raw.rs` — each stage wrapped in
  `if flags.X { … }`; the DCP apply path forwards the two DCP flags into
  `color::dcp::apply_if_available`. Two new tests:
  `each_flag_change_alters_output` (proves the flags reach their stage)
  and `defaults_match_bare_load_image` (reproducibility).
- `apps/desktop/src/color/dcp/mod.rs` — `apply_if_available` gains
  `apply_hue_sat: bool` + `apply_look: bool` params.
- `apps/desktop/src/navigation/preloader.rs` — `Preloader::start` takes
  flags; `set_raw_flags` updates them.
- `apps/desktop/src/settings/persistence.rs` — new `raw` and
  `custom_dcp_dir` fields with `#[serde(default)]`.
- `apps/desktop/src/commands.rs` — `SetRawPipelineFlags` and
  `SetCustomDcpDir` variants.
- `apps/desktop/src/app/executor.rs` — two new arms.
- `apps/desktop/src/app.rs` — `raw_flags` field; `apply_raw_flag_change`
  and `apply_custom_dcp_dir_change` helpers; `apply_custom_dcp_dir` free
  function for the env-var sync.
- `apps/desktop/src/settings/panels/raw.rs` (new) — RAW panel, AppKit
  delegate, 10 toggles + custom DCP picker + reset button.
- `apps/desktop/src/settings/window.rs` — sidebar entry; panel wiring;
  `switch_settings_section` knows "raw".
- `docs/notes/` — this file + `raw-roadmap.md`.
- `CHANGELOG.md` — single `### Added` bullet.
