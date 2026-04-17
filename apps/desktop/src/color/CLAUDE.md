# Color

ICC color management end to end: transform, display profile detection (macOS-only),
and the Settings → Color panel.

| File                 | Purpose                                                                            |
| -------------------- | ---------------------------------------------------------------------------------- |
| `mod.rs`             | `color::State { icc_enabled, match_display, relative_col, display_icc }` + re-exports |
| `transform.rs`       | `moxcms`-based ICC transform, `srgb_icc_bytes`, `profiles_match` byte-equality     |
| `profiles.rs`        | Linear Rec.2020 `ColorProfile` factory + Rec.2020↔XYZ matrices (RAW working space) |
| `tone_curve.rs`      | Default tone curve applied to linear RAW output (Phase 2.3). Hermite knees + midtone line, analytical |
| `sharpen.rs`         | Capture sharpening for RAW output (Phase 2.4). Separable Gaussian unsharp mask on display-space RGBA8, σ = 0.8 px, amount = 0.3 |
| `delta_e.rs`         | CIE76 Delta-E for comparing RGBA8 buffers (used by RAW pipeline regression tests)  |
| `display_profile.rs` | macOS: `CGDisplayCopyColorSpace`, `CAMetalLayer` colorspace, screen-change observer |
| `settings_panel.rs`  | Settings → Color: ICC color management + Color match display + Relative colorimetric |

## State

`App.color: color::State` owns this feature's fields: `icc_enabled`, `match_display`,
`relative_col`, and `display_icc` (the target ICC bytes, defaults to sRGB). Updated
on setting changes and on `AppCommand::DisplayChanged`.

## ICC flow

Display ICC bytes: `CGDisplayCopyColorSpace` (at startup) → `App.display_icc` →
`Preloader` (as `Arc<Vec<u8>>`) → per-rayon-task closure →
`decoding::load_image_cancellable` → `color::transform_icc`. On display change,
`AppCommand::DisplayChanged` re-queries, flushes the cache, and re-decodes.

## Decisions

- **moxcms over lcms2.** ~5.5× faster on Apple Silicon (NEON SIMD). Pure Rust, simpler
  build. Full comparison in `docs/notes/icc-level-2-display-color-management.md`.
- **Byte-equality skip.** Source-ICC == target-ICC ⇒ zero-cost no-op. Images without
  embedded profile are assumed sRGB.
- **Perceptual intent by default.** "Relative colorimetric" toggle is opt-in for
  photographers comparing specific color values.

## Gotchas

- **`srgb_icc_bytes()` panics on non-macOS.** Reads
  `/System/Library/ColorSync/Profiles/sRGB Profile.icc` which is macOS-only.
- **`CGColorRef`/`CGColorSpaceRef` confuse `msg_send!`**. They're `*const c_void`
  (encoded `^v`); ObjC expects `^{CGColor=}`. Use raw `objc2::ffi::objc_msgSend` +
  `transmute`. See `display_profile.rs`.
- **Display profile fallback.** If `CGDisplayCopyColorSpace` returns null (headless,
  SSH, CI), falls back to `/System/Library/ColorSync/Profiles/sRGB Profile.icc`.
