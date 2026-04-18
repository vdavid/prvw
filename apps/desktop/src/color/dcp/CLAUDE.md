# DCP (Digital Camera Profile)

Per-camera color refinement that picks up where a generic 3×3 matrix can't.
Applies a `ProfileHueSatMap` 3D LUT in linear-light HSV: cyclic hue axis,
clamped sat / val axes, trilinear interpolation between grid points. Runs
**post-highlight-recovery, pre-tone-curve** in the RAW pipeline.

| File                 | Purpose                                                                   |
| -------------------- | ------------------------------------------------------------------------- |
| `mod.rs`             | Public API (`apply_if_available`, `DcpSource`, re-exports + test helpers) |
| `parser.rs`          | Parses standalone `.dcp` files (TIFF-like `IIRC` container)               |
| `embedded.rs`        | Reads the same profile tags straight from a DNG's IFD (`from_dng_tags`)   |
| `apply.rs`           | Trilinear 3D LUT application in HSV (rayon-parallel)                      |
| `discovery.rs`       | Filesystem `.dcp` discovery + camera-identity matching                    |
| `illuminant.rs`      | Scene color-temperature estimate + dual-illuminant `HueSatMap` blend (3.4)|
| `bundled.rs`         | Bundled RawTherapee DCP collection loader (Phase 3.5)                     |
| `family_aliases.rs`  | Fuzzy camera family fallback alias table (Phase 3.5)                      |

## Discovery paths and search order

A profile reaches the applier via one of these tiers, tried in order:

1. **Embedded** (`embedded::from_dng_tags`, Phase 3.3). The DNG's own main
   IFD carries the profile tags. Every Pixel, Samsung Galaxy, and iPhone
   ProRAW file ships one; Adobe DNG Converter also bakes one in. The camera
   manufacturer chose this profile, so it's the most trustworthy source.
2. **Filesystem exact** (`discovery::find_dcp_for_camera`, Phase 3.2). No
   embedded profile — try a standalone `.dcp` under `$PRVW_DCP_DIR` or
   Adobe Camera Raw's default directory
   (`~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/`).
   Matching is by `UniqueCameraModel` (case-insensitive, whitespace-tolerant)
   or `ProfileCalibrationSignature` as a fallback.
3. **Bundled exact** (`bundled::find_bundled_dcp`, Phase 3.5). Try the 161
   RawTherapee community profiles packed into the binary at build time
   (~10 MB zstd blob). No user setup required.
4. **Fuzzy family alias** (`family_aliases::aliases_for`, Phase 3.5). For
   each curated alias of the camera, repeat tiers 2 and 3. First hit wins.
   Logs at INFO so users see the substitution.
5. **None** — fall back to the default pipeline.

All paths produce the same `Dcp` / `HueSatMap` types, so `apply.rs` runs
unchanged on any source.

## Precedence

Embedded wins. Always. When a DNG has embedded profile tags AND a
matching filesystem DCP exists, the embedded profile is picked.

Users who genuinely want to override can set
`PRVW_DISABLE_EMBEDDED_DCP=1` — the pipeline then falls through to the
filesystem path. That knob is aimed at QA comparisons and advanced users;
normal operation never needs it.

## Fuzzy-alias matches don't apply DCP color

`apply_if_available` takes an `allow_fuzzy: bool` parameter. The RAW
pipeline passes `false`: when the only hit is via `FAMILY_ALIASES`, the
function logs an INFO line and returns `None` without running
`HueSatMap` or `LookTable`. The reason is sensor spectral response.
Same-family bodies (ILCE-5000 vs. ILCE-6000) have different CFA
filters, so a HueSatMap calibrated on one body pushes reds / magentas
/ skin tones into wrong places on another. The ProfileToneCurve is
skipped too because fuzzy matches never reach the tone-curve stage —
`dcp_info` is `None` for those.

Users who want the fuzzy profile applied anyway can drop an exact-match
DCP under `$PRVW_DCP_DIR` or set `allow_fuzzy = true` from a test
harness.

## Log output

INFO level, once per successful match:

```
RAW applied EMBEDDED DCP 'Google Embedded Camera Profile' for camera 'Google Pixel 6 Pro' on …
RAW applied filesystem DCP 'SONY ILCE-7M3' for camera 'Sony ILCE-7M3' on …
RAW applied bundled DCP 'SONY ILCE-7M3' for camera 'Sony ILCE-7M3' on …
DCP: no exact match for 'Sony ILCE-5000'; using compatible profile 'SONY ILCE-6000' from bundled collection
```

The source label (`EMBEDDED`, `filesystem`, `bundled`, `bundled (alias)`,
`filesystem (alias)`) is logged as part of the standard INFO line.

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

## Phase 3.5 — bundled collection + fuzzy family matching

See `docs/notes/raw-support-phase3.md` for the full write-up.

- **Bundled** (`bundled.rs`): 161 RawTherapee community DCPs, zstd-packed
  at build time by `build.rs` from `apps/desktop/build-assets/dcps/`. The
  blob decompresses once on first lookup (a `OnceLock`), then lives in
  memory for the process lifetime (~83 MB). Binary size delta: +9.7 MB.
- **Fuzzy aliases** (`family_aliases.rs`): a `FAMILY_ALIASES` const table
  maps cameras to known-compatible substitutes. Conservative — only entries
  with same-sensor or same-family evidence. Extend via PR.

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

- **Unit**: `embedded.rs` has 7 tests. `parser.rs` has 7 tests. `apply.rs`
  has 9. `bundled.rs` has 4 (bundled count, known camera, unknown camera,
  count sanity). `family_aliases.rs` has 4 (aliases for known camera, unknown,
  normalization, table integrity).
- **Ignored smoke**:
  - `decoding::raw::tests::embedded_dcp_smoke` — decodes sample2.dng
    (Pixel 6 Pro) with and without the embedded profile.
  - `decoding::raw::tests::dcp_smoke` — filesystem path on a Sony ARW.

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
- **The bundled blob decompresses to ~83 MB on first DCP lookup.** This
  is expected and cached for the process lifetime. It's only allocated
  when a non-embedded DCP is needed. Images with embedded profiles (all
  smartphones, Adobe DNG Converter output) never trigger it.
- **`discovery.rs::normalize` is `pub(super)`.** It was previously `fn`
  (module-private). Promoted to `pub(super)` in Phase 3.5 so `bundled.rs`
  and `family_aliases.rs` can reuse it without duplication.
