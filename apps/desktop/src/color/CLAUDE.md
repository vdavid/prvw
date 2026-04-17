# Color

ICC color management end to end: transform, display profile detection (macOS-only),
and the Settings → Color panel.

| File                 | Purpose                                                                            |
| -------------------- | ---------------------------------------------------------------------------------- |
| `mod.rs`             | `color::State { icc_enabled, match_display, relative_col, display_icc }` + re-exports |
| `transform.rs`       | `moxcms`-based ICC transform, `srgb_icc_bytes`, `profiles_match` byte-equality     |
| `profiles.rs`        | Linear Rec.2020 `ColorProfile` factory + Rec.2020↔XYZ matrices (RAW working space) |
| `tone_curve.rs`      | Default tone curve applied to linear RAW output (Phase 2.3 / 2.5a). Hermite knees + midtone line; since 2.5a shaped on **luminance only** (Rec.2020 weights), so hue and chroma are preserved through the highlight shoulder. Since 2.5b: `apply_default_tone_curve` wraps a parametric `apply_tone_curve(rgb, midtone_anchor)` around the `DEFAULT_MIDTONE_ANCHOR` const |
| `saturation.rs`      | Global saturation boost for RAW output (Phase 2.5a). Scales chroma around luma in linear Rec.2020 by `(1 + DEFAULT_SATURATION_BOOST)`. Already parametric. Preserves hue and luminance. Since 2.5b rerun: `DEFAULT_SATURATION_BOOST = 0.0` (no-op) — Preview.app doesn't want an extra global lift on top of the tone curve |
| `highlight_recovery.rs` | Highlight recovery for RAW output (Phase 3.1). Post-exposure, pre-tone-curve. Near-clip pixels desaturate toward their luminance via smoothstep between `DEFAULT_THRESHOLD` (0.95) and `DEFAULT_CEILING` (1.20) in linear Rec.2020. Preserves hue direction (no inversion) and perceived brightness (Y is invariant under the mix). Avoids the magenta/cyan drift that appears when one channel clips while the others keep rising. Parametric entry point `apply_highlight_recovery` exposed for future per-camera tuning |
| `dcp/` (Phase 3.2) | Adobe Digital Camera Profile support — opt-in per-camera color refinement. Submodules: `parser.rs` reads the `.dcp` TIFF-like binary, `apply.rs` applies `ProfileHueSatMap` as a trilinearly-interpolated 3D LUT in HSV (cyclic hue, clamped sat/val), `discovery.rs` searches `$PRVW_DCP_DIR` + Adobe Camera Raw's install dir and matches by `UniqueCameraModel`. Runs post-highlight-recovery, pre-tone-curve. Silent no-op when no DCP matches. Deferred: `LookTable`, `ProfileToneCurve`, dual-illuminant interpolation, `ForwardMatrix` swap (see `docs/notes/raw-support-phase3.md`) |
| `sharpen.rs`         | Capture sharpening for RAW output (Phase 2.4 / 2.5a). Separable Gaussian unsharp mask on display-space RGBA8 acting on **luminance only** (Rec.709 weights), σ = `DEFAULT_SIGMA`, amount = `DEFAULT_AMOUNT`; avoids color fringes at colored edges. Since 2.5b: `sharpen_rgba8_inplace` wraps a parametric `sharpen_rgba8_inplace_with(rgba, w, h, sigma, amount)` around the default constants |
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
- **Parametric + default wrapper pattern (Phase 2.5b).** `tone_curve.rs` and
  `sharpen.rs` both expose a parametric entry point (`apply_tone_curve`,
  `sharpen_rgba8_inplace_with`) alongside the `apply_default_*` /
  `sharpen_rgba8_inplace` wrappers. Production stays on the wrappers; the
  `raw-tune` dev example and future Phase 3 per-camera DCP apply code call
  the parametric ones. Keeps the runtime pipeline path unchanged while
  setting up scene- or sensor-specific values without a second fork of the
  math.

## Gotchas

- **`srgb_icc_bytes()` panics on non-macOS.** Reads
  `/System/Library/ColorSync/Profiles/sRGB Profile.icc` which is macOS-only.
- **`CGColorRef`/`CGColorSpaceRef` confuse `msg_send!`**. They're `*const c_void`
  (encoded `^v`); ObjC expects `^{CGColor=}`. Use raw `objc2::ffi::objc_msgSend` +
  `transmute`. See `display_profile.rs`.
- **Display profile fallback.** If `CGDisplayCopyColorSpace` returns null (headless,
  SSH, CI), falls back to `/System/Library/ColorSync/Profiles/sRGB Profile.icc`.
