# DCP (Digital Camera Profile)

Per-camera color refinement that picks up where a generic 3×3 matrix can't.
Applies a `ProfileHueSatMap` 3D LUT in linear-light HSV: cyclic hue axis,
clamped sat / val axes, trilinear interpolation between grid points. Runs
**post-highlight-recovery, pre-tone-curve** in the RAW pipeline.

| File            | Purpose                                                                   |
| --------------- | ------------------------------------------------------------------------- |
| `mod.rs`        | Public API (`apply_if_available`, `DcpSource`, re-exports + test helpers) |
| `parser.rs`     | Parses standalone `.dcp` files (TIFF-like `IIRC` container)               |
| `embedded.rs`   | Reads the same profile tags straight from a DNG's IFD (`from_dng_tags`)   |
| `apply.rs`      | Trilinear 3D LUT application in HSV (rayon-parallel)                      |
| `discovery.rs`  | Filesystem `.dcp` discovery + camera-identity matching                    |
| `illuminant.rs` | Scene color-temperature estimate + dual-illuminant `HueSatMap` blend (3.4)|

## Two discovery paths, one applier

A profile reaches the applier from one of two sources:

1. **Embedded** (`embedded::from_dng_tags`, Phase 3.3). The DNG's own main
   IFD carries the profile tags — `ProfileHueSatMapDims`,
   `ProfileHueSatMapData1/2`, `ProfileHueSatMapEncoding`, and so on. Every
   Pixel, Samsung Galaxy, and iPhone ProRAW file ships one; Adobe DNG
   Converter also bakes one in when you convert a non-DNG RAW. The camera
   manufacturer chose this profile, so it's the most trustworthy source.
2. **Filesystem** (`discovery::find_dcp_for_camera`, Phase 3.2). No
   embedded profile — fall back to a standalone `.dcp` under
   `$PRVW_DCP_DIR` or Adobe Camera Raw's default directory
   (`~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/`).
   Matching is by `UniqueCameraModel` (case-insensitive,
   whitespace-tolerant) or `ProfileCalibrationSignature` as a fallback.

Both paths produce the same `Dcp` / `HueSatMap` types, so `apply.rs` runs
unchanged on either.

## Precedence

Embedded wins. Always. When a DNG has embedded profile tags AND a
matching filesystem DCP exists, the embedded profile is picked. The
manufacturer's profile is part of the file's authoritative color
description; overriding it with a third-party DCP is almost never the
right call.

Users who genuinely want to override can set
`PRVW_DISABLE_EMBEDDED_DCP=1` — the pipeline then falls through to the
filesystem path. That knob is aimed at QA comparisons and advanced users;
normal operation never needs it.

## Log output

INFO level, once per successful match:

```
RAW applied EMBEDDED DCP 'Google Embedded Camera Profile' for camera 'Google Pixel 6 Pro' on …
RAW applied filesystem DCP 'SONY ILCE-7M3' for camera 'Sony ILCE-7M3' on …
```

The source label is the first word (`EMBEDDED` vs. `filesystem`) so
`grep` stays trivial.

## Phase 3.4 — LookTable + ToneCurve + dual-illuminant

Three Phase 3.2-deferred items landed together in Phase 3.4:

- **`LookTable`**: second HueSatMap-shaped 3D LUT applied after
  `HueSatMap`, captures Adobe's "Look" refinement on top of the neutral
  calibration. Parsed from tags 50981/50982/51108.
- **`ProfileToneCurve`**: per-camera tone curve applied **instead of**
  our default Hermite S-curve when the profile ships one (tag 50940).
  Luminance-only, piecewise-linear, same RGB-scale pattern as the
  default. INFO log spells out which curve ran.
- **Dual-illuminant blend**: when a DCP ships `HueSatMap1` and
  `HueSatMap2` at different calibration illuminants, blend them by the
  scene's color temperature. Compromise fidelity: a one-shot WB-ratio
  scene-temp estimate drives a linear blend between the two maps.
  `illuminant.rs` owns the math.

Single-map DCPs, DCPs without a LookTable, and DCPs without a tone
curve all continue to no-op through the relevant passes — zero
regression on Phase 3.3 fixtures.

## What's still deferred

- **`ForwardMatrix1/2` swap.** DCP forward matrices target ProPhoto D50;
  we target linear Rec.2020. A correct swap needs a full chromatic-
  adaptation re-pipe.
- **Iterative CCT convergence.** The current dual-illuminant blend
  uses a one-shot `temp ≈ 7000 − 2000 × (R/G − 1)` estimate from the
  raw white balance. The DNG spec's full procedure iterates
  `ForwardMatrix1/2` + `AsShotNeutral` until a self-consistent
  temperature converges. Good enough for a viewer today; a later
  refinement.

## Tests

- **Unit**: `embedded.rs` has 7 tests (happy path, missing dims, missing
  both data maps, only Data2 present, size mismatch, double fallback,
  full-metadata round-trip). `parser.rs` has its own 7 tests for the
  standalone-file format. `apply.rs` has 9 for the LUT math.
- **Ignored smoke**:
  - `decoding::raw::tests::embedded_dcp_smoke` — decodes sample2.dng
    (Pixel 6 Pro) with and without the embedded profile and asserts a
    > 1 % byte difference. Set `PRVW_EMBEDDED_DCP_SMOKE_DUMP=/some/dir`
    to also emit `without-embedded.png` and `with-embedded.png`.
  - `decoding::raw::tests::dcp_smoke` — same pattern for the filesystem
    path on a Sony ARW with a `/tmp/prvw-dcp-test/` DCP.

## Gotchas

- **`raw.dng_tags` isn't populated for DNGs**; it's populated by rawler's
  RAF (Fuji) decoder only. For a real DNG, the profile tags sit on the
  TIFF root IFD, accessible via `decoder.ifd(WellKnownIFD::Root)` or
  `WellKnownIFD::VirtualDngRootTags`. `raw::collect_dng_profile_tags`
  merges all three stores into a single `HashMap` that `from_dng_tags`
  consumes.
- **Rawler's `Value` is already endian-normalised.** The typed vectors
  inside `Value::Long`, `Value::Float`, etc. are native-endian; no
  byte-swap work happens in `embedded.rs`.
