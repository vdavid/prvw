# Imaging (load → decode → color → cache → navigate)

Everything about turning a file on disk into pixels ready for GPU upload, plus the
directory-level navigation that keeps the preloader fed.

The module is named `imaging` (not `image`) to avoid a name clash with the external
`image` crate used by `loader.rs` for non-JPEG decoding.

| File            | Purpose                                                                           |
| --------------- | --------------------------------------------------------------------------------- |
| `loader.rs`     | Format-specific decoders (`zune-jpeg` for JPEG, `image` crate for PNG/GIF/BMP/TIFF/WebP), ICC profile extraction, cancellation support |
| `color.rs`      | ICC transform via `moxcms` (source profile → target), perceptual + relative colorimetric intents, byte-equality skip |
| `preloader.rs`  | Rayon thread pool (`min(4, cores-1)`) for parallel background decoding, `ImageCache` with LRU eviction (512 MB budget) |
| `directory.rs`  | Scan parent dir for supported extensions, sort, track current position, yield preload-range indices |

## Key patterns

- **ICC flow.** Display ICC bytes: `CGDisplayCopyColorSpace` (at startup) → `App.display_icc` →
  `Preloader` (as `Arc<Vec<u8>>`) → per-rayon-task closure → `loader::load_image_cancellable`
  → `decode_jpeg` / `decode_generic` → `color::transform_icc`. On display change, the
  `DisplayChanged` command re-queries, flushes the cache, and re-displays.
- **moxcms is ~5.5× faster than lcms2** on Apple Silicon (NEON SIMD). See
  `docs/notes/icc-level-2-display-color-management.md` for benchmarks.
- **Byte-equality skip.** If source ICC bytes match target ICC bytes, the transform is
  skipped (zero cost for P3-on-P3, sRGB-on-sRGB, etc.). Images with no embedded profile
  are assumed sRGB.
- **Preloader threading.** CPU-bound decoding on `std::thread` + rayon — no `tokio`.
  Communication via `std::sync::mpsc`. An in-flight `HashSet` prevents duplicate work.

## Gotchas

- **`zune-jpeg` in debug builds.** Its SIMD path is painfully slow without optimizations.
  `Cargo.toml` sets `[profile.dev.package.zune-jpeg] opt-level = 3`.
- **ICC extraction order with the `image` crate.** `ImageReader::into_decoder()` returns
  `impl ImageDecoder`. `icc_profile()` takes `&mut self`; `DynamicImage::from_decoder()`
  consumes the decoder. Call `icc_profile()` first, then `from_decoder()`.
- **`srgb_icc_bytes()` panics on non-macOS.** It reads
  `/System/Library/ColorSync/Profiles/sRGB Profile.icc` which is macOS-only. Cross-platform
  support will need a fallback embedded sRGB profile.
