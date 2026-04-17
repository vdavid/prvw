# Decoding

Decode image files to RGBA8, extract the embedded ICC profile, apply EXIF orientation,
and hand a `DecodedImage` off to the renderer.

| File             | Purpose                                                                          |
| ---------------- | -------------------------------------------------------------------------------- |
| `mod.rs`         | Public API: `DecodedImage`, `load_image`, `load_image_cancellable`, `is_supported_extension` |
| `dispatch.rs`    | `Backend` enum + extension-to-backend mapping                                    |
| `jpeg.rs`        | Fast JPEG path via `zune-jpeg` (SIMD)                                            |
| `raw.rs`         | Camera RAW via `rawler` (DNG, CR2, CR3, NEF, ARW, ORF, RAF, RW2, PEF, SRW)       |
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

## RAW pipeline (Phase 2.4)

`raw.rs` bypasses rawler's default `Calibrate`/`CropDefault`/`SRgb` stages so we
can keep the intermediate wide-gamut:

1. Rawler runs `Rescale → Demosaic → CropActiveArea` only, producing a
   3-channel float buffer in camera RGB.
2. `raw.rs::camera_to_linear_rec2020` applies white balance and
   `cam → linear Rec.2020` (via the camera's D65 matrix composed with
   `XYZ → linear Rec.2020`). No clip.
3. Default crop.
4. `raw.rs::apply_exposure` lifts the linear buffer by the baseline EV picked
   by `baseline_exposure_ev` (DNG `BaselineExposure` tag first, fallback
   +0.5 EV, clamped to [-2, +2]). Linear-space multiply so relative luminance
   stays correct.
5. `color::tone_curve::apply_default_tone_curve` shapes the linear buffer
   with a mild filmic S-curve: shadow Hermite → midtone line (slope 1.08,
   anchored at 0.25) → highlight shoulder. Analytical, monotonic,
   endpoint-preserving. Closes the "flat look" gap against Preview.app.
6. `color::transform_f32_with_profile` hands the buffer to moxcms for the
   linear-Rec.2020 → display-ICC conversion in f32. Clamp to [0, 1] on the
   way out to RGBA8.
7. `color::sharpen::sharpen_rgba8_inplace` runs a mild unsharp mask on the
   display-space RGBA8 buffer: separable Gaussian blur (σ = 0.8 px,
   7 taps) + `output += (output - blurred) * 0.3`. Closes the "slightly
   soft" gap against Preview.app and Lightroom. Post-ICC rather than
   pre-ICC so we match the perceptual response of the gamma-encoded
   buffer and avoid halos from linear-space unsharp on bright edges.

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
