# RAW support — Phase 4

Lens correction — distortion, transverse chromatic aberration (TCA), and
vignetting — for non-DNG RAWs. Complements Phase 3 color work with pure
geometry corrections from the LensFun community database.

## Phase 4.0 — integration (done, 2026-04-17)

Wires the `lensfun` crate (pure-Rust port of LensFun, local path dependency
at `../../../lensfun-rs`) into Prvw's RAW pipeline. Around 280 LoC in Prvw
proper plus ~170 LoC inlined into the `raw-dev-dump` example; the crate
handles the hard math (4D calibration interpolation, Newton iteration for
poly3/poly5 distortion inverses, per-channel TCA kernels, the vignetting
polynomial).

### What was integrated

Three correction passes, each gated independently by calibration
availability for the matched lens:

- **Distortion** — `ptlens` (4th-order closed-form), `poly3`, and `poly5`
  (Newton iteration) models. For every output pixel we ask the modifier
  what source pixel the light came from, then bilinear-sample the input
  buffer.
- **TCA** — `linear` and `poly3` models. Red and blue planes get shifted
  independently relative to green to close chromatic fringing at colored
  edges.
- **Vignetting** — `pa` model. Multiplies each pixel by
  `1 / (1 + k1·r² + k2·r⁴ + k3·r⁶)` so the darkened corners lift back to
  neutral. No resample needed.

### Pipeline position

```
rawler demosaic
  → camera_to_linear_rec2020
  → DNG OpcodeList3 (DNG only — existing Phase 3.0)
  → lens correction via lensfun-rs         ← NEW
  → default crop → exposure → highlight recovery → DCP → tone → saturation
  → ICC → sharpen
```

Same slot as DNG's `OpcodeList3::WarpRectilinear`. That's the spec-correct
place for post-color lens corrections: the color matrix has already turned
the sensor buffer into a standard working space, so geometry corrections
see coordinates that match the manufacturer's calibration reference.

### DNG-OpcodeList3 interaction

`apply_opcode_list3` returns `bool` indicating whether a `WarpRectilinear`
fired. The decoder threads that into a `warp_rectilinear_applied` flag; the
Phase 4 step checks it before running. When true, we `log::debug!` a skip
line and leave the buffer alone — the manufacturer already corrected
distortion, and re-applying LensFun's correction would warp straight lines
back the other way.

Result:
- **iPhone ProRAW / Pixel ProRAW / Adobe-converted DNGs**: WarpRectilinear
  fires in OpcodeList3 → Phase 4 skips. No double correction.
- **Sony ARW / Canon CR2 / CR3 / Nikon NEF / Fuji RAF**: no opcodes → Phase
  4 runs if the lens is in LensFun's DB.

### Matching logic

Prvw hands LensFun:

- `raw.camera.make` + `raw.camera.model` (rawler-normalised short form —
  `"SONY"` + `"ILCE-5000"`, etc.)
- EXIF `lens_model` (`"E PZ 16-50mm F3.5-5.6 OSS"`)
- EXIF `focal_length` (mm), `fnumber` (f-number), `subject_distance` (m —
  falls back to 1000 m when missing, matching LensFun's "effectively
  infinity" semantics)

`Database::find_cameras` and `find_lenses` do fuzzy string scoring. We take
the top match and build a `Modifier` from it. When any required field is
missing or empty, we `log::debug!` and no-op.

### Settings toggle

`RawPipelineFlags::lens_correction` defaults to `true`. Sits under a new
"Geometry" section in Settings → RAW, alongside the 10 existing Phase 3.7
toggles. Flipping it off flushes the image cache and re-decodes through the
same `AppCommand::SetRawPipelineFlags` path.

## E2E smoke test findings

Run with
`RUST_LOG=info cargo test --release lens_correction_smoke -- --ignored --nocapture`
(fixtures in `/tmp/raw/` — outside the repo). Output capture:

### sample1.arw — Sony ILCE-5000 + E PZ 16-50mm f/3.5-5.6 OSS @ 16mm f/3.5

- LensFun matched the camera (1 hit) and the lens (1 hit).
- All three correction passes fired: distortion (D), TCA (T), vignetting (V).
- 52 994 307 of 79 268 480 output bytes differ between
  `lens_correction=true` and `lens_correction=false` — that's 66.86 % of
  the buffer.
- Visual: visible barrel-distortion rectification (the shoreline and
  horizon now read straight; before, they bowed out at the edges). Corner
  brightness clearly lifted from the vignetting pass. Classical "framing"
  darkening in the corners that comes from the inverse-warp resample —
  the pixels that used to live at the image edge now get mapped slightly
  outside the frame.

### sample2.dng — Google Pixel 6 Pro @ 2.35mm f/2.2

- DNG `OpcodeList3` carries 3 opcodes, one of which is `WarpRectilinear`.
- Phase 4 detects the flag and skips (`log::debug!` line:
  "lens_correction: skipped for … (DNG OpcodeList3 WarpRectilinear
  already applied)").
- Output is **bit-identical** with `lens_correction=true` vs. `=false` —
  exactly what we want. Test assertion enforces this.

### sample3.arw — Sony ILCE-5000 + E PZ 16-50mm f/3.5-5.6 OSS @ 16mm f/3.5

- Same match as sample1.arw.
- 56 299 156 of 79 268 480 bytes differ (71.03 %).
- Visual: zigzag pattern on the wall reads straighter after correction.
  Right-hand bed edge now linear.

### raw-dev-dump stages

The `raw-dev-dump` example gained a `before-lens-correction` + an
`after-lens-correction` stage so pipeline regressions show up visually.
For the two ARWs these PNGs differ; for the DNG they're byte-identical
(the WarpRectilinear-already-fired short-circuit).

## Known limitations

- **Lenses not in the DB silent no-op.** LensFun's community database has
  ~1,543 lenses but it's not exhaustive. Non-popular glass from third-party
  makers, adapter + manual-lens combos, or very new releases will miss.
- **No `subject_distance` from most files.** EXIF rarely carries a
  trustworthy focus distance; we default to 1000 m ("infinity") so the
  vignetting interpolation picks far-focus calibrations. Close-focus
  vignetting may be slightly off.
- **Fuzzy string match over-permissive.** `find_lenses` returns a sorted
  list and we take the top hit; occasionally a close-enough family member
  wins when the exact lens isn't there. Logs the matched name so the
  substitution is auditable.
- **Not yet supported by rawler's EXIF extraction**: any file where
  rawler doesn't expose `lens_model` (some older Nikon NEF, many Pentax
  PEFs). These silently no-op.
- **No perspective correction.** The lensfun-rs port covers v1 of the
  LensFun correction set (distortion, TCA, vignetting). Perspective
  correction is stretch-scope (v1.1) in the crate and hasn't been wired
  through to Prvw yet.
- **Corner framing after distortion rectification.** The inverse warp
  maps output pixels to source coordinates that may lie outside the
  input rectangle at the corners. We clamp to the nearest edge pixel,
  which produces a subtle radial smear in the outermost rows/columns.
  Editors fix this with an auto-crop to the largest inscribed
  rectangle; viewers don't.
