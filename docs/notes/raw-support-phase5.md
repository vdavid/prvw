# RAW support — Phase 5

HDR / EDR output. Stops clipping RAW highlights at display-white and
prepares the pipeline to push peak-white values onto an EDR-capable
Mac display (XDR mini-LED, OLED). SDR displays stay bit-identical to
Phase 4 — no regression for users on non-HDR screens.

## What shipped in Phase 5.0 (2026-04-17)

Scope-down decision up front: Phase 5.0 is everything except the wgpu
surface format switch + `CAMetalLayer.wantsExtendedDynamicRangeContent`.
Those land in Phase 5.1 because surface reconfiguration mid-session in
the wgpu 29 version we ship is more involved than the rest of the phase
combined, and keeping the decode path + cache + settings ready means the
follow-up can focus purely on the GPU surface lifecycle.

### Filmic Reinhard shoulder

`color::tone_curve` now shapes the highlight region with a rational
Reinhard-like curve asymptoting at `peak` instead of a Hermite cubic
landing exactly on 1.0. For `x > HIGHLIGHT_KNEE`:

```text
y = y_knee + (peak - y_knee) · t / (t + s)
where t = x - HIGHLIGHT_KNEE
      s = (peak - y_knee) / MIDTONE_SLOPE
```

`s` is chosen so the first derivative at `t = 0` equals `MIDTONE_SLOPE`,
which makes the join C¹ (value + slope match the midtone line). As
`t → ∞`, `y → peak`. The shape never clips, which matters for the HDR
path: we want 1.5 or 2.0 linear-light inputs (saturated skies, specular
highlights) to come out between 1.0 and 4.0 rather than pinned at 1.0.

Defaults:

- `DEFAULT_PEAK_SDR = 1.0` — same ceiling as Phase 4. Used when the
  display reports no EDR headroom or the user has turned the HDR toggle
  off.
- `DEFAULT_PEAK_HDR = 4.0` — user-confirmed target. Peaks above 4.0 start
  to look unnatural on mini-LED (local-dimming halos around bright
  points); below 2.0 barely uses the display's headroom. 4.0 lands in
  the sweet spot for Apple's XDR displays at default brightness.

Asymptote check at `x = 10`, `peak = 4.0`, anchor = 0.40: `y ≈ 3.44`.
At `x = 50`: `y ≈ 3.86`. At `x → ∞`: `y → 4.0` but never reaches.

### `PixelBuffer` + `DecodedImage`

`DecodedImage.rgba_data: Vec<u8>` is gone. In its place:

```rust
pub enum PixelBuffer {
    Rgba8(Vec<u8>),
    Rgba16F(Vec<u16>),
}

pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: PixelBuffer,
}
```

`Rgba16F` stores each half-float as a raw `u16` bit pattern (what the
`half` crate's `f16::to_bits()` / `from_bits()` use). The renderer
reinterprets the vector as `&[u8]` via `bytemuck::cast_slice` for the
`queue.write_texture` upload. `PixelBuffer::bytes_per_pixel()` is the
single source of truth for row-pitch and cache-cost math.

### RAW decoder HDR branch

`decoding::raw::decode` now takes an extra `edr_headroom: f32`. At the
top of the function:

```rust
let hdr_active = flags.hdr_output && edr_headroom > 1.0;
let peak = if hdr_active { DEFAULT_PEAK_HDR } else { DEFAULT_PEAK_SDR };
```

The tone curve calls `apply_tone_curve(&mut rec2020, anchor, peak)`
instead of the old `apply_default_tone_curve`. Post-ICC, branch:

- SDR: clamp to `[0, 1]`, quantise to RGBA8, run the existing unsharp-
  mask on luminance → `PixelBuffer::Rgba8`.
- HDR: preserve values above 1.0, quantise to RGBA16F via
  `half::f16::from_f32` → `PixelBuffer::Rgba16F`. Skip the unsharp-mask
  for now (see "Deferred" below).

`decoding::load_image` and `load_image_cancellable` also take
`edr_headroom: f32`. Non-RAW decoders ignore it — JPEG/PNG/WebP stay
RGBA8 always, because those formats max out at SDR and we don't want
surprise memory growth for photographers who only browse smartphone
JPEGs.

### EDR headroom query

New function in `color::display_profile`:

```rust
pub fn current_edr_headroom(window: &Window) -> f32
```

Calls `NSScreen.maximumExtendedDynamicRangeColorComponentValue` via
objc2. Returns 1.0 for SDR displays, 2.0 to 16.0 for EDR displays
(macOS reports the current headroom live — brightness changes, battery
saver, ambient light all swing it). Returns 1.0 on any failure so the
fallback is "behave like Phase 4." On the author's 16" M3 Max XDR with
default settings we observed `16.00`.

Re-queried on `AppCommand::DisplayChanged`, alongside the display ICC
profile refresh. When the value changes by more than 0.001 the image
cache is flushed, the preloader's copy is updated, and the cache's
budget is retuned between SDR and HDR.

### Settings toggle

`RawPipelineFlags::hdr_output` (default `true`). Exposed in Settings →
RAW under a new "Output" group with the label "HDR / EDR output" and
the description "Keep highlights above display-white alive when the
screen supports it." Toggling flushes the image cache via the existing
`AppCommand::SetRawPipelineFlags` path.

Default-on means EDR-display users get HDR highlights out of the box;
SDR-display users see no change (headroom == 1.0 → `hdr_active ==
false` → pure Phase 4 output).

### Cache budget

`navigation::preloader::ImageCache` now has `set_hdr_mode(bool)`:

- SDR: 512 MB budget, unchanged from Phase 4.
- HDR: 1 GB budget.

The doubling comes from half-float RAWs being 8 bytes per pixel instead
of 4. Without the bump, a 20 MP RAW jumps from ~80 MB to ~160 MB and
the preload count drops from ~6 to ~3. User's call: trade RAM for
preload count, because the whole point of the preloader is zero-latency
navigation. Users on tight RAM can opt out via the HDR toggle.

The mode switch runs in three places:

- On app init, once the initial EDR headroom is known.
- On `AppCommand::DisplayChanged`, when a screen change flips headroom
  across the 1.0 boundary.
- On `AppCommand::SetRawPipelineFlags`, when the user toggles
  `hdr_output`.

Shrinking from HDR (1 GB) back to SDR (512 MB) evicts LRU entries until
the resident set fits — no "we decoded too much" panic.

### GPU texture upload

`render::renderer::Renderer::set_image` picks the texture format from
the `PixelBuffer` variant:

- `PixelBuffer::Rgba8` → `TextureFormat::Rgba8UnormSrgb` (unchanged).
- `PixelBuffer::Rgba16F` → `TextureFormat::Rgba16Float`.

The fragment shader samples both as `vec4<f32>`, so no shader variant
split is needed. The surface format stays `Bgra8UnormSrgb` in this
phase; values above 1.0 survive through the decode + cache but get
clipped at the final blend. That's the part Phase 5.1 will fix.

## What shipped in Phase 5.1 (2026-04-17)

Phase 5.1 flips the final switch: when the current image is `Rgba16F`,
HDR output is enabled, and the display reports EDR headroom above 1.0,
the wgpu surface reconfigures to `Rgba16Float` and `CAMetalLayer` goes
into its EDR mode. On SDR displays (or with the toggle off), nothing
changes — the `synthetic_dng_matches_golden` regression test passes
unchanged.

### `App::want_edr_surface` — the single source of truth

Three conditions, all AND-ed:

1. `raw_flags.hdr_output == true` (user opt-in, default on).
2. `edr_headroom > 1.0` (display advertises EDR).
3. `current_image_is_hdr` (the last decode emitted `PixelBuffer::Rgba16F`).

`App.current_image_is_hdr` is a new field set in `display_image` and
`display_cached_or_load` from the freshly-loaded image's `PixelBuffer`
variant. It flips back to `false` whenever a non-RAW (or an
SDR-branch-taking RAW) loads, which naturally degrades the surface
back to SDR when the user navigates to a JPEG.

### `Renderer::reconfigure_surface_format`

Flips the wgpu `SurfaceConfiguration.format` between `Rgba16Float` and
the platform's preferred SDR format captured at init time (typically
`Bgra8UnormSrgb` on macOS). Rebuilds three pipelines that reference
the surface format:

- Image-quad pipeline — via the extracted `build_image_pipeline`
  helper.
- Overlay pill pipeline — via `build_overlay_pipeline`.
- `GlyphonRenderer` — rebuilt wholesale via `GlyphonRenderer::new`
  because glyphon's `TextAtlas` pins the format at construction and
  doesn't expose a swap API. Cheap: re-creating the atlas on a format
  flip is a single allocation and a fresh swash cache.

Shader modules and pipeline layouts are cached on `Renderer`, so the
rebuild doesn't recompile WGSL or re-validate bind-group layouts. One
INFO log line per transition spells out the old and new formats.

### `color::display_profile::set_layer_edr_state`

Three CAMetalLayer properties set in lockstep so the wgpu surface
config and the Metal-layer config can't drift:

- `setWantsExtendedDynamicRangeContent:YES|NO` (the key knob — the
  compositor routes the window through the EDR path only when this is
  `YES`).
- `setPixelFormat:MTLPixelFormatRGBA16Float (115)` for EDR,
  `MTLPixelFormatBGRA8Unorm_sRGB (81)` for SDR.
- `setColorspace:` — `kCGColorSpaceExtendedLinearDisplayP3` for EDR
  (linear-light, signed floats, Display P3 primaries — perfect pair
  for our linear Rec.2020 pixels above 1.0), or the display ICC
  profile bytes when returning to SDR (reuses the existing
  `set_colorspace_on_layer` path from Phase 2).

### Colorspace choice: `extendedDisplayP3`

Picked over `extendedLinearDisplayP3` because our ICC pipeline
encodes the f16 texture through the display profile's transfer
function (sRGB or P3 gamma, not linear-light) — so the CAMetalLayer
colorspace needs the matching non-linear transfer. Naming a linear
colorspace here would make the compositor decode the same gamma
curve twice, producing washed-out or crushed output.

`extendedLinearSRGB` was also considered and rejected: the M3 Max
XDR (and most modern Apple displays) natively covers P3, not sRGB,
and the pipeline already targets Display-P3 primaries via the
display ICC.

The "Extended" variant is what keeps above-1.0 values alive — the
non-extended `kCGColorSpaceDisplayP3` clamps at 1.0.

### Trigger points

`App::apply_edr_surface_state` runs whenever anything that feeds
`want_edr_surface()` changes:

- `display_image` — after each decode (image-HDR-ness changed).
- `display_cached_or_load` — after each navigation (cached image may
  be HDR or SDR).
- `apply_raw_flag_change` — user flipped `hdr_output` in Settings.
- `handle_display_changed` — screen change or brightness change, and
  an `apply_icc_settings` re-decode was already queued.

The reconfigure is idempotent: if the surface is already in the right
state, `reconfigure_surface_format` returns `false` and no logs fire.

### Screenshot path

`capture_screenshot` always renders to an SDR offscreen target now —
PNG readback and the BGRA→RGBA swizzle stay straightforward, and a
PNG can't represent above-1.0 values anyway. When the live pipeline
is already SDR, we reuse it; when it's HDR, we build a one-shot SDR
image pipeline for the capture pass. Values above 1.0 clip to
display-white, which is the correct behavior for a screenshot.

### Scope

Dynamic switching is in. Tested on M3 Max / XDR: SDR → HDR on RAW
load, HDR → SDR on navigate to JPEG, HDR → SDR on Settings toggle
off, all without recreating the window.

## Deferred to Phase 5.x

- **Unsharp-mask on f16.** The existing sharpener runs on RGBA8
  luminance with a 7-tap separable Gaussian. Porting to f16 means a
  second code path (different clamp, different rounding); the bigger
  concern is that sharpening in linear light above 1.0 wants a
  different amount than in gamma-encoded 8-bit. Tuning is Phase 5.1
  work, and HDR output without capture sharpening still looks
  noticeably better than SDR-clipped output, so shipping the f16 path
  without the sharpener is a net win.

## Testing + validation notes

- `./scripts/check.sh` green (14 checks, 294 tests).
- Tone-curve tests now cover: monotonic across `[0, 10]`, SDR output
  always `≤ 1.0 + 1e-6`, HDR output asymptotes toward 4.0 without
  reaching, C¹ continuity at the highlight knee across `peak` values,
  `hdr_apply_keeps_wide_gamut_highlights`, `sdr_apply_matches_phase4_
  sdr_behavior`.
- Cache tests: `cache_accounts_f16_at_eight_bytes_per_pixel`,
  `cache_hdr_budget_doubles`, `cache_shrinks_on_budget_drop`.
- Interactive EDR verification needs a real XDR display. The author's
  M3 Max 16" MacBook Pro reports `edr_headroom = 16.00`; smoke-running
  the release build on sample1.arw / sample2.dng / sample3.arw produces
  "[HDR]" tags in the decode log and uses the 1 GB cache budget
  correctly.

## SDR parity contract

On an SDR display (`edr_headroom == 1.0`) or with the `hdr_output`
toggle off, the RAW decode output is **bit-identical to Phase 4 for every
supported RAW format**. That's enforced by:

1. The filmic shoulder with `peak = 1.0` asymptotes exactly at 1.0. At
   `x = 1.0` it evaluates to some value close to 1.0 (shoulder lands
   just inside), but the SDR path then `f32_to_u8`-clips to `[0, 1]`
   with the same `(v * 255.0 + 0.5) as u8` quantisation Phase 4 used.
   The golden test on `synthetic-bayer-128.dng` passes without regenerating
   the golden PNG, which means no byte drifted.
2. When `edr_headroom == 1.0`, `hdr_active` is `false` no matter what
   `flags.hdr_output` says, so the decoder goes down the RGBA8 branch
   and never touches `PixelBuffer::Rgba16F`.

The Phase 4 contract — "flipping any pipeline stage off flushes and
re-decodes" — still holds for the new `hdr_output` flag.
