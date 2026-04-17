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
