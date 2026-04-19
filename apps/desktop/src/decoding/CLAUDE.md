# Decoding

Decode image files to RGBA8, extract the embedded ICC profile, apply EXIF orientation,
and hand a `DecodedImage` off to the renderer.

| File             | Purpose                                                                          |
| ---------------- | -------------------------------------------------------------------------------- |
| `mod.rs`         | Public API: `DecodedImage`, `PixelBuffer` (Phase 5 RGBA8/RGBA16F), `load_image`, `load_image_cancellable`, `is_supported_extension` |
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
2b. **Phase 4.0: lens correction via `lensfun-rs`**
    (`color::lens_correction::apply_lens_correction`). Uses the bundled
    LensFun community database (~1,041 cameras, 1,543 lenses) to apply
    distortion (ptlens / poly3 / poly5), TCA (linear / poly3), and
    vignetting (pa model) in place on the linear Rec.2020 buffer.
    Matches on `raw.camera.make/model` + EXIF
    `lens_model/focal_length/fnumber`. Silent no-op when any of those is
    missing, when the lens isn't in the DB, or when the DNG's
    `OpcodeList3::WarpRectilinear` already fired (the caller tracks
    `warp_rectilinear_applied` from step 2a and skips to avoid double
    correction). Order within: vignetting → distortion → TCA.
3. Default crop.
3a. **Phase 6.1: chroma noise reduction**
    (`color::chroma_denoise::apply_default_chroma_denoise`). Splits
    linear Rec.2020 RGB into Y + Cb + Cr (Rec.2020 weights), blurs
    Cb and Cr with a small separable Gaussian (`σ = 1.5 px`, 11 taps),
    reconstructs RGB. Luma stays sharp; chroma smooths. Matches the
    silent chroma-NR default in Preview.app and Affinity. Toggleable
    via `flags.chroma_denoise` (default `true`); per-image output at
    `false` is bit-identical to pre-6.1.
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
7. **Color conversion, branching on HDR.** SDR path (`hdr_active == false`):
   `color::transform_f32_with_profile` hands the buffer to moxcms for the
   linear-Rec.2020 → user's display-ICC conversion in f32. Clamp to [0, 1]
   on the way out to RGBA8. HDR path (`hdr_active == true`):
   `color::profiles::rec2020_to_linear_display_p3_inplace` applies a direct
   3×3 matrix Rec.2020 → linear Display P3 with **no clipping** (Phase 5.2
   — moxcms clips at 1.0 which eats HDR headroom, and the `CAMetalLayer`
   is pinned to `extendedLinearDisplayP3` on EDR anyway so a direct matrix
   to that target is the natural fit). HDR brightness gain
   (`flags.hdr_gain`, default 2.0) multiplies the buffer before the matrix
   to push scene-white content into the EDR headroom — without it, HDR
   output reads timidly SDR-bright rather than "HDR-bright" against Preview.
7b. **Phase 5: HDR branch.** If the caller's `edr_headroom > 1.0` and
    `flags.hdr_output == true`, skip the `[0, 1]` clamp and quantise the
    f32 buffer into `PixelBuffer::Rgba16F` (half-floats via the `half`
    crate), preserving values above 1.0 for the EDR-capable compositor.
    Sharpening still fires on this branch via
    `color::sharpen::sharpen_rgba16f_inplace` (same luminance-only
    unsharp mask as the 8-bit path, computed in f32 with no `[0, 1]`
    clamp so above-white highlights survive). Otherwise the SDR path
    below fires.
7c. **Phase 6.2: clarity (local contrast).**
    `color::clarity::apply_clarity_rgba8_inplace_with` runs a larger-
    radius (`σ ≈ 10 px`) separable-Gaussian unsharp mask on **luminance
    only**, lifting midtone features — shape silhouettes, textures — that
    survive display downscaling so the image reads crisper at every zoom
    level. Same math as capture sharpening, different defaults. Runs
    before step 8 so the order is midtone lift → fine-edge sharpening.
    The HDR branch at 7b calls `apply_clarity_rgba16f_inplace_with` on the
    half-float buffer with the same semantics. Toggleable via
    `flags.clarity` (default `true`).
8. `color::sharpen::sharpen_rgba8_inplace` runs a mild unsharp mask on
   **luminance only** (Rec.709 weights) of the display-space RGBA8 buffer:
   separable Gaussian blur (σ = 0.8 px, 7 taps) on Y in f32, unsharp-mask
   formula on Y, then rescale RGB by `Y_out / Y_in`. Post-ICC rather than
   pre-ICC so we match the perceptual response of the gamma-encoded buffer
   and avoid halos. Luminance-only avoids the color fringes per-channel
   sharpening produces at colored edges.

The linear Rec.2020 `ColorProfile` is built programmatically in
`color::profiles::linear_rec2020_profile`. No bundled ICC file.

## HDR / EDR flow (Phase 5 + 5.2)

Whether a given decode lands in HDR or SDR is decided once, up front, in
`raw::decode`:

```
hdr_active = flags.hdr_output && edr_headroom > 1.0
```

`edr_headroom` is threaded in from `app.rs` (queried via
`color::display_profile::current_edr_headroom` on `NSScreen`). `App` also
tracks `current_image_is_hdr` so the window's `CAMetalLayer` can be flipped
between SDR and EDR modes per-decode — `edr_should_be_active` fuses all three
inputs (flag, headroom, image-is-HDR).

Once inside `raw::decode`, `hdr_active` gates four different behaviors:

1. **Tone-curve peak.** `DEFAULT_PEAK_HDR` (4.0) vs `DEFAULT_PEAK_SDR` (1.0)
   picks the filmic shoulder's asymptote. HDR lets above-1.0 content live;
   SDR clips at display-white.
2. **Color conversion** (see step 7 in the pipeline list above). Direct
   Rec.2020 → linear Display P3 matrix for HDR; moxcms → user's display
   ICC for SDR.
3. **HDR brightness gain.** `flags.hdr_gain` (default 2.0) multiplies the
   post-tone-curve buffer before the matrix, pushing scene-white into EDR
   headroom. SDR path ignores the knob. See
   `docs/notes/raw-support-phase5.md` Phase 5.2 for rationale.
4. **Output format.** `rec2020_to_rgba16f` (preserving above-1.0) vs
   `rec2020_to_rgba8` (clamp + quantize). Sharpening / clarity pick the
   f16 or 8-bit variant to match.

The renderer side observes the same decision via `App::edr_should_be_active`
and calls `color::display_profile::apply_edr_state` to flip the
`CAMetalLayer` between `kCGColorSpaceExtendedLinearDisplayP3 + RGBA16Float`
(HDR) and the user's display ICC + `BGRA8Unorm_sRGB` (SDR). Two diagnostic
peak-value log lines (`peak linear value` pre-conversion,
`peak post-ICC` post-conversion, plus `peak f16` on the HDR-output summary
line) make the decision audit-able without a debugger — if HDR looks wrong,
the logs tell you whether the pipeline clipped, quantized, or rendered the
above-1.0 range away.

## Per-stage timing (Phase 6.4)

`raw::decode` instruments every pipeline stage with a `StageTimings` helper
(private to `raw.rs`). Each stage mark emits one `log::debug!` line, and a
summary `log::debug!` line fires at the end:

```
RAW pipeline stages [293.2 ms total]: raw_image=4.4, opcode1=0.0, rescale=2.2,
  opcode2=0.0, demosaic=34.7, cam_matrix=6.0, opcode3=0.0, lens=83.0, crop=5.4,
  chroma_nr=49.3, exposure=2.8, hl_recovery=2.7, dcp=38.9, tone=7.7,
  hdr_diag_pre=2.6, saturation=2.4, color_conv=6.2, hdr_diag_post=2.2,
  to_rgba16f=4.7, clarity=14.0, sharpen=21.4, hdr_diag_f16=2.3 for …
```

Flag-gated stages with near-zero elapsed confirm their gate fired (useful
sanity check). Turn it on with `RUST_LOG=prvw::decoding::raw=debug`.

### Typical warm-decode budget on a 20 MP ARW

Reference numbers: `/tmp/raw/sample3.arw` (Sony α7R IV, 5456×3632 ≈ 20 MP),
Apple Silicon M3 Max, release build, all RAW flags at their defaults (HDR
output active on an EDR-capable display, so the numbers below include the
half-float path). Re-run measurements for a fresh cold decode by flipping
Settings → General → "Preload next/prev images" off and restarting — the
preloader otherwise warms caches and decode-time drops.

| Stage              | Warm ms | Notes |
| ------------------ | ------: | ----- |
| `raw_image`        |     4.4 | Inside rawler: file parse + mosaic extraction |
| `opcode1`          |     0.0 | DNG only; ARW no-op |
| `rescale`          |     2.2 | rawler's black-level subtract + linear rescale |
| `opcode2`          |     0.0 | DNG only; ARW no-op |
| `demosaic`         |    34.7 | Inside rawler; bilinear (no Markesteijn) |
| `cam_matrix`       |     6.0 | camera → linear Rec.2020 (WB + color matrix in one pass) |
| `opcode3`          |     0.0 | DNG only; ARW no-op |
| `lens`             |    83.0 | LensFun distortion + TCA + vignetting (SIMD, 6.3) |
| `crop`             |     5.4 | Default crop |
| `chroma_nr`        |    49.3 | σ=1.5 blur on Cb/Cr (SIMD, 6.1) |
| `exposure`         |     2.8 | Baseline EV lift |
| `hl_recovery`      |     2.7 | Highlight recovery |
| `dcp`              |    38.9 | DCP HueSatMap + LookTable (trilinear 3D LUT) |
| `tone`             |     7.7 | Filmic shoulder curve on luminance |
| `hdr_diag_pre`     |     2.6 | Info-level diagnostic (0.1 ms when info off) |
| `saturation`       |     2.4 | Global chroma scale |
| `color_conv`       |     6.2 | Rec.2020 → linear Display P3 matrix (HDR) or moxcms (SDR) |
| `hdr_diag_post`    |     2.2 | Info-level diagnostic (0.1 ms when info off) |
| `to_rgba16f`       |     4.7 | f32 → half-float quantize |
| `clarity`          |    14.0 | σ=10 downsample fast path (6.4 — was 144 ms pre-6.4) |
| `sharpen`          |    21.4 | σ=0.8 luminance-only unsharp |
| `hdr_diag_f16`     |     2.3 | Info-level diagnostic (0.1 ms when info off) |
| **total**          | **293** | Warm cache; cold decode ≈ 650 ms |

**How to read the table:** numbers are guide-rail order-of-magnitude, not a
contract. They shift with image resolution, scene content, cache state,
thermal throttling, and flag combinations. The absolute values matter less
than the *ranking* — `lens`, `chroma_nr`, `dcp`, `demosaic`, `sharpen`,
`clarity` are the six stages that dominate. Demosaic and rawler parse live
inside the `rawler` crate; everything else is our code.

**If you're optimizing:** start by reading the DEBUG summary line on your
own hardware / input. If you see a stage taking ≥ 2× the value here, that
specific stage is the suspect. If every stage is proportionally higher, it
probably isn't our code — check `log::info!` lines above for clues (e.g.
"RAW applied DCP" means DCP matched and the trilinear 3D LUT ran). See
Phase 6.2 (clarity downsample) and 6.3 (lens-correction SIMD) in
`docs/notes/raw-support-phase6.md` for examples of how past per-stage
wins were found and landed.

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
