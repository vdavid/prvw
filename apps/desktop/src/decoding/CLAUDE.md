# Decoding

Decode image files to RGBA8, extract the embedded ICC profile, apply EXIF orientation,
and hand a `DecodedImage` off to the renderer.

| File             | Purpose                                                                          |
| ---------------- | -------------------------------------------------------------------------------- |
| `mod.rs`         | Public API: `DecodedImage`, `load_image`, `load_image_cancellable`, `is_supported_extension` |
| `dispatch.rs`    | `Backend` enum + extension-to-backend mapping                                    |
| `jpeg.rs`        | Fast JPEG path via `zune-jpeg` (SIMD)                                            |
| `raw.rs`         | Camera RAW via `rawler` (DNG, CR2, CR3, NEF, ARW, ORF, RAF, RW2, PEF, SRW)       |
| `dng_opcodes.rs` | DNG `OpcodeList1/2/3` parsing + application (Phase 3.0)                          |
| `generic.rs`     | Fallback path via the `image` crate (PNG, GIF, WebP, BMP, TIFF)                  |
| `orientation.rs` | EXIF orientation parsing and in-place pixel-buffer rotation                      |

## Key patterns

- **Backend dispatch is extension-based.** `dispatch::pick_backend(ext)` picks the
  decoder; `is_supported_extension` is the gate the directory scanner uses. Adding
  a format means: teach `dispatch` about its extensions, add a backend module, and
  match it in `mod::decode_with`.
- **Cancellation.** `load_image_cancellable` takes an `AtomicBool`, checked while
  reading the file (every 64 KB chunk) and again before dispatching to a backend.
  Returns `Err("cancelled")` if the flag flips. Used by the preloader so navigating
  away aborts in-flight work.
- **ICC profile first, pixels second.** See the gotcha below.

## Gotchas

- **ICC extraction ordering (`generic.rs`).** `ImageReader::into_decoder()` returns
  `impl ImageDecoder`. `icc_profile()` takes `&mut self`, and
  `DynamicImage::from_decoder()` consumes the decoder. So call `icc_profile()`
  first, then `from_decoder()`. Reversing won't compile.
- **`zune-jpeg` in debug builds is unusably slow.** `apps/desktop/Cargo.toml` sets
  `[profile.dev.package.zune-jpeg] opt-level = 3` to fix this. Without it, cold
  startup on a 20 MP photo takes seconds.
- **Unknown EXIF orientation values get logged and ignored.** The spec defines 1–8;
  cameras occasionally write garbage. We pass the buffer through unchanged rather
  than guess.
- **RAW orientation lives on the decoder's metadata, not `RawImage`.** Rawler
  hard-codes `RawImage.orientation` to `Normal`; the real EXIF value is on
  `decoder.raw_metadata(...).exif.orientation`. `raw.rs` reads it there and hands
  it back to the dispatcher so `apply_orientation` can rotate the developed
  buffer. Because of this, the RAW backend is the only one that supplies its own
  orientation instead of going through the shared `parse_exif_orientation` over
  the outer file bytes.
- **Fujifilm X-Trans demosaic is bilinear only.** Rawler ships a simple X-Trans
  bilinear demosaic, not Markesteijn. Usable in a viewer but less detailed than
  what dedicated RAW tools produce.
- **Rawler applies `LinearizationTable` (tag 50712) itself.** Look in
  `rawler-0.7.2/src/decoders/mod.rs::641` — the generic raw path
  dither-interpolates every raw pixel through the table when the tag is
  present, so we don't need a second pass in `dng_opcodes.rs`. The Phase 3.0
  investigation (see `docs/notes/raw-support-phase3.md`) confirmed this.
- **DNG opcode coordinates are raw-image-absolute.** For CFA opcodes (OpcodeList1
  and 2) that's fine because we apply them before active-area crop. For
  `OpcodeList3` on the demosaiced+cropped buffer we currently ignore the active-
  area origin; every fixture we test starts the active area at (0, 0), so no
  shift is needed today. Cameras with a nonzero origin would miscrop post-color
  opcodes — tracked as Phase 3.x future work.

## RAW pipeline (Phase 2.5a + Phase 3.0)

`raw.rs` bypasses rawler's default `Calibrate`/`CropDefault`/`SRgb` stages so we
can keep the intermediate wide-gamut:

1. Rawler's `raw_image` extracts the mosaic and metadata.
1a. **Phase 3.0: DNG `OpcodeList1` applied** (`dng_opcodes.rs`). Pre-
    linearization gain maps and bad-pixel fixes on the CFA mosaic.
    Silent no-op for non-DNG files and for DNGs without the tag.
1b. `raw.apply_scaling()` — rawler's black-level subtract + [0, 1] linear
    rescale, split out so we can slip the next step in between.
1c. **Phase 3.0: DNG `OpcodeList2` applied**. Post-linearization, pre-
    demosaic CFA-level gain maps. This is where iPhone ProRAW stashes its
    per-Bayer-phase lens-shading correction (4 `GainMap`s with pitch 2×2).
1d. Rawler's remaining develop steps: `Demosaic → CropActiveArea`,
    producing a 3-channel float buffer in camera RGB.
2. `raw.rs::camera_to_linear_rec2020` applies white balance and
   `cam → linear Rec.2020` (via the camera's D65 matrix composed with
   `XYZ → linear Rec.2020`). No clip.
2a. **Phase 3.0: DNG `OpcodeList3` applied**. Post-color `WarpRectilinear`
    for lens distortion, `GainMap` if any. On iPhone ProRAW, the
    optional `WarpRectilinear` fires here.
3. Default crop.
4. `raw.rs::apply_exposure` lifts the linear buffer by the baseline EV picked
   by `baseline_exposure_ev` (DNG `BaselineExposure` tag first, fallback
   +0.5 EV, clamped to [-2, +2]). Linear-space multiply so relative luminance
   stays correct.
4a. **Phase 3.1: highlight recovery**
    (`color::highlight_recovery::apply_default_highlight_recovery`). Pixels
    whose brightest channel exceeds `DEFAULT_THRESHOLD` (0.95) are blended
    toward their own luminance via a smoothstep between threshold and
    `DEFAULT_CEILING` (1.20) in linear Rec.2020. In-gamut pixels pass
    through untouched. Keeps bright skies and specular highlights from
    drifting magenta/cyan when one channel clips while the other two
    keep rising. Runs post-exposure so it catches exposure-induced
    overflow too, and pre-tone-curve so the curve sees a hue-consistent
    input.
4b. **Phase 3.2 / 3.3 / 3.4: DCP** (`color::dcp::apply_if_available`).
    Finds a DCP matching the camera — either embedded in a DNG (Phase
    3.3, preferred) or a standalone `.dcp` under `$PRVW_DCP_DIR` /
    Adobe Camera Raw (Phase 3.2, fallback). Applies its
    `ProfileHueSatMap` as a trilinearly-interpolated 3D LUT in
    linear-light HSV. Since Phase 3.4: dual-illuminant profiles blend
    `HueSatMap1` + `HueSatMap2` by the scene's estimated color
    temperature (compromise fidelity), and a `ProfileLookTable` fires
    after the HueSatMap when present. Silent no-op for files without a
    matching profile. Still deferred: `ForwardMatrix` swap, full
    iterative CCT convergence. See `docs/notes/raw-support-phase3.md`.
5. **Tone curve.** When the active DCP carries a `ProfileToneCurve`
   (Phase 3.4), `color::tone_curve::apply_tone_curve_lut` runs it via
   piecewise-linear interpolation on the pixel's Rec.2020 luminance,
   then scales RGB uniformly by `Y_out / Y_in` — same hue-preserving
   pattern as the default curve. Otherwise `apply_default_tone_curve`
   shapes luminance with a mild filmic S-curve: shadow Hermite →
   midtone line (slope 1.08, anchored at 0.25) → highlight shoulder.
   Either way hue and chroma are preserved through the shoulder.
6. `color::saturation::apply_saturation_boost` scales each pixel's chroma
   around its luminance axis by `(1 + 0.08)` in linear Rec.2020 space.
   Preserves hue and luminance; adds the "vibrancy" Apple/Affinity bake in
   via per-camera tuning tables.
7. `color::transform_f32_with_profile` hands the buffer to moxcms for the
   linear-Rec.2020 → display-ICC conversion in f32. Clamp to [0, 1] on the
   way out to RGBA8.
8. `color::sharpen::sharpen_rgba8_inplace` runs a mild unsharp mask on
   **luminance only** (Rec.709 weights) of the display-space RGBA8 buffer:
   separable Gaussian blur (σ = 0.8 px, 7 taps) on Y in f32, unsharp-mask
   formula on Y, then rescale RGB by `Y_out / Y_in`. Post-ICC rather than
   pre-ICC so we match the perceptual response of the gamma-encoded buffer
   and avoid halos. Luminance-only avoids the color fringes per-channel
   sharpening produces at colored edges.

The linear Rec.2020 `ColorProfile` is built programmatically in
`color::profiles::linear_rec2020_profile`. No bundled ICC file.

## Testing the RAW pipeline

The RAW pipeline has two kinds of tests:

- **Unit tests in `raw.rs`** — malformed bytes, cancellation. Cheap, always on.
- **Golden regression test in `mod.rs` (`synthetic_dng_matches_golden`).** Runs
  the full `load_image` path on `tests/fixtures/raw/synthetic-bayer-128.dng`
  (a tiny synthetic DNG, ~33 KB) and compares the RGBA8 output against a
  checked-in golden PNG via CIE76 Delta-E. Tolerances: mean < 0.5, max < 3.0.
  macOS-gated because `load_image` reads the system sRGB ICC profile.

Regenerating the golden after an intentional pipeline change:

```sh
cd apps/desktop
PRVW_UPDATE_GOLDENS=1 cargo test synthetic_dng_matches_golden
```

Delta-E lives in `src/color/delta_e.rs`. The `raw-dev-dump` example
(`cargo run --example raw-dev-dump -- <raw_path>`) dumps per-stage PNGs for
visual inspection during pipeline development.

See `tests/fixtures/raw/README.md` for fixture details and
`docs/notes/raw-support-phase2.md` for the pipeline-evolution plan.
