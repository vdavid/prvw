# ICC Level 2: display-aware color management

Decision date: 2026-04-15. Implemented 2026-04-16.

## Context

Prvw Level 1 converted embedded ICC profiles (Adobe RGB, ProPhoto, Display P3) to sRGB before GPU upload. This is
correct on sRGB displays but wastes the gamut on wide-gamut monitors. Both of our test displays are P3-class:

- **DELL G3223Q** (external, main): primaries close to Display P3, 524-byte custom ICC profile
- **MacBook Pro Liquid Retina XDR** (built-in): primaries exactly match Apple's Display P3 profile, 4064-byte
  factory-calibrated profile with LUT correction

Level 2 converts to the display's actual ICC profile instead. On P3 displays, P3 images show their full gamut instead
of being clamped to sRGB.

## ICC color management levels (for reference)

- **Level 1**: source profile -> sRGB (minimum honest). Covers the real pain: Adobe RGB photos stop looking washed out.
- **Level 2**: source profile -> display profile (proper macOS color management). P3 displays show the full range.
- **Level 3**: full color management with rendering intents, soft proofing, CMYK preview. Photoshop/Lightroom territory,
  not what prvw is about.

## Decision 1: how to tell the GPU/compositor about the output color space

**Chosen: set `CAMetalLayer.colorspace`, keep `Rgba8UnormSrgb` textures.**

| Option | Description | Verdict |
|---|---|---|
| **A: `CAMetalLayer.colorspace` (chosen)** | Transform source -> display profile in moxcms, upload as 8-bit, set the Metal layer's colorspace so macOS knows what we're outputting. Works because P3 and sRGB share the same EOTF (sRGB transfer function). | Minimal code change. No shader, pipeline, or screenshot changes. |
| **B: `Rgba8Unorm` textures** | No automatic sRGB decode. Handle linearization in the shader. | More shader complexity, pipeline changes, screenshot swizzle changes. Only needed if the display has a non-sRGB transfer function, which basically doesn't exist in practice. |
| **C: `Rgba16Float` textures** | Linear-light 16-bit intermediate. Textbook correct for wide gamut. | 2x texture memory (24MP: 96MB -> 192MB), halves the 512 MB LRU cache. CPU-side 8-bit-to-float conversion. Overkill when source images are 8-bit anyway. |

**Why A**: Display P3 and sRGB share the same transfer function (sRGB EOTF). The only difference is the color primaries
(gamut). So `Rgba8UnormSrgb` still applies the correct nonlinear-to-linear conversion for P3 content. We just need to
tell the compositor "these pixels are P3" via `CAMetalLayer.colorspace`. This avoids touching the shader, the render
pipeline, the surface format, and the screenshot capture code.

**Risk**: if a display has a transfer function that differs from sRGB's (rare, but theoretically possible with
custom calibration profiles that include complex TRC curves), the hardware sRGB decode in the shader would be slightly
wrong. In practice, every consumer display on the market uses an sRGB-compatible transfer function. If this ever matters,
we'd upgrade to option B or C.

## Decision 2: sRGB images on P3 displays

**Chosen: always transform every image to the display profile. No early exit for sRGB.**

| Option | Description | Verdict |
|---|---|---|
| **A: Always transform (chosen)** | Remove the sRGB-specific early exit. Even sRGB images get transformed to the display profile. | Always correct. One code path. ~45ms cost on the first image, hidden by preloader for adjacent images. |
| **B: Set layer colorspace per image** | sRGB layer for sRGB content, P3 layer for P3 content. | Possible flicker on navigation. More state to track. |
| **C: Keep sRGB skip, accept inaccuracy** | sRGB content on a P3 display without transform. | If `CAMetalLayer.colorspace` is P3, the compositor interprets sRGB pixels as P3 — colors are slightly undersaturated. Defeats the purpose. |

**Why A**: the generalized skip is byte-equality (`profiles_match`). If source ICC bytes == target ICC bytes, the
transform is skipped with zero cost. This handles P3-on-P3, sRGB-on-sRGB, and any other identity case. Non-identity
transforms cost ~45ms for 24MP, which the preloader masks for all but the very first image.

Images without embedded profiles are assumed sRGB (the web/camera default). On a P3 display, they get an sRGB -> P3
transform. This is correct: without it, the compositor would interpret their pixels as P3, making colors look slightly
wrong.

## Decision 3: how to get the display ICC profile from macOS

**Chosen: `CGDisplayCopyColorSpace()` + `CGColorSpaceCopyICCData()` via raw C FFI.**

The canonical CoreGraphics API. Two function calls, returns raw ICC bytes that plug straight into moxcms. We already have
the FFI patterns (`objc2`, `msg_send!`) throughout the codebase. The display ID is resolved by matching the window's
monitor position against `CGDisplayBounds` for all active displays.

Rejected alternative: reading the profile file path from ColorSync preferences. Fragile, undocumented path format, and
doesn't handle dynamic profiles (True Tone, Night Shift).

## Decision 4: display profile caching

**Chosen: query per image load, no caching.**

`CGDisplayCopyColorSpace` takes microseconds, negligible compared to the ~45ms transform and ~218ms JPEG decode. No
invalidation logic needed. If profiling ever shows it matters (it won't), add `OnceLock` + invalidation. Not worth the
code now.

## Decision 5: detecting window moved to a different display

**Chosen: `NSWindowDidChangeScreenNotification` via objc2.**

| Option | Description | Verdict |
|---|---|---|
| **A: Poll `window.current_monitor()` on `Resized`** | Check on every resize event. | Misses moves between same-DPI monitors that don't trigger resize. |
| **B: `NSWindowDidChangeScreenNotification` (chosen)** | Native notification, fires exactly when the window's center point crosses to a different screen. | Correct by definition. One-time registration, no polling. |
| **C: Don't detect, require reopen** | User reopens the image on the new screen. | Bad UX for multi-monitor setups. Not what a "platform-native" app should do. |

**Why B**: it fires after the window has moved, so `[window screen]` already returns the new screen. The notification
is window-specific (not app-level), so it fires for our window only. Implementation: `define_class!` delegate with a
`screenDidChange:` method that sends `AppCommand::DisplayChanged` via the global event loop proxy.

Apple's docs: "Posted whenever the window's current screen changes. The notification is sent when the window's center
point moves to a different screen." This means during a drag, the window briefly straddles two screens, and the
notification fires at the center-point threshold. Same behavior as Preview, Photos, and other macOS apps.

## Decision 6: preloader cache on display change

**Chosen: flush the entire cache.**

| Option | Description | Verdict |
|---|---|---|
| **A: Flush cache (chosen)** | `ImageCache::clear()`. The preloader refills it quickly. | Simple. Display changes are rare. |
| **B: Cache raw pixels, re-transform** | Store pre-transform pixels alongside transformed ones. | ~2x cache memory. More complexity in `DecodedImage`. For a rare event. |
| **C: Tag cache entries with target profile** | Keep multiple versions per image. | Complex eviction. Doubles effective cache size. |

**Why A**: Display changes happen when the user physically drags a window between monitors. This is rare. The preloader
runs on background threads and refills the cache in seconds. The user pays ~263ms for the current image (re-decode +
re-transform) and the rest are preloaded while they're looking at it.

## Decision 7: fallback when display profile is unavailable

**Chosen: fall back to sRGB (Level 1 behavior).**

If `CGDisplayCopyColorSpace` or `CGColorSpaceCopyICCData` returns null (headless, SSH, CI), the display ICC defaults to
the macOS system sRGB profile (`/System/Library/ColorSync/Profiles/sRGB Profile.icc`). This is the same behavior as
Level 1. Zero risk.

The `srgb_icc_bytes()` function loads this file once via `OnceLock` and panics if it's missing. It's always present on
macOS. Cross-platform support will need an embedded fallback sRGB profile.

## Decision 8: newtype wrappers for color spaces

**Chosen: no newtypes. Use clear function names and comments.**

The Logical/Physical pixel newtypes work because they wrap scalars (`f64`, `u32`) that flow through dozens of functions
across multiple modules. Color space would wrap `Vec<u8>` that flows through exactly two functions (`decode_jpeg` /
`decode_generic` -> `transform_icc`) in one module before becoming "display-ready." The type boundary exists in a
~10-line span.

Named color space types (`SrgbPixels`, `DisplayP3Pixels`, etc.) don't work either because ICC profiles are continuous,
not discrete. Every factory-calibrated display has a unique profile. You'd need an `Other(Vec<u8>)` catch-all that most
real profiles fall into.

If Level 3 ever introduces multiple intermediate spaces (working space -> soft-proof simulation -> display output),
newtypes would start preventing real bugs. For Level 2, the clear function name (`transform_icc`) and a comment on
`DecodedImage` documenting "pixels are in the display's color space" are sufficient.

## Performance

Benchmarked on Apple M3 Max, release build, 24MP (6000x4000) Adobe RGB JPEG:

| Step | Time |
|---|---|
| JPEG decode (zune-jpeg) | ~218ms |
| ICC transform (moxcms, NEON SIMD) | ~45ms |
| **Total** | **~263ms** |

Level 2's per-pixel cost is the same as Level 1 — only the LUT contents change based on the target profile. The
preloader handles adjacent images in background threads, so the user only pays this on the first image.

## Key files

| File | Role |
|---|---|
| `apps/desktop/src/imaging/color.rs` | `transform_icc()`, `profiles_match()`, `srgb_icc_bytes()` |
| `apps/desktop/src/platform/macos/display_profile.rs` | CoreGraphics FFI, `CAMetalLayer` colorspace, screen change observer |
| `apps/desktop/src/imaging/loader.rs` | ICC extraction, passes `target_icc` through decode pipeline |
| `apps/desktop/src/imaging/preloader.rs` | Stores display ICC, passes to decode tasks, `ImageCache::clear()` |
| `apps/desktop/src/app.rs` | `display_icc` field, `handle_display_changed()`, init in `initialize_viewer()` |
| `apps/desktop/src/commands.rs` | `AppCommand::DisplayChanged` |
| `apps/desktop/tests/color_management.rs` | Integration tests (P3 vs sRGB, identity transform) |
| `apps/desktop/tests/fixtures/p3_red_64x64.jpg` | 64x64 test fixture with Display P3 profile |
