# Render (infrastructure: wgpu)

Not a feature. This is the GPU rendering scaffolding. Features like `zoom` (which owns `ViewState`) plug transforms into
the renderer's uniform buffer via `crate::zoom::view::TransformUniform`.

| File                | Purpose                                                                                       |
| ------------------- | --------------------------------------------------------------------------------------------- |
| `gpu.rs`            | Which backend and which adapter this platform asks wgpu for, as a pure policy                 |
| `display_hdr.rs`    | `DisplayHdr { capable, headroom_now }` read out of wgpu's `DisplayHdrInfo`, pure and testable |
| `image_pipeline.rs` | The image quad's shader, bind group layouts, per-frame uniform, and upload; GPU parity tests  |
| `renderer.rs`       | `wgpu` instance/device/surface, two pipelines (image quad, overlay pill), screenshot readback |
| _(HDR)_             | `display_hdr` and `configure_surface` live in `renderer.rs`; see the sections below           |
| `text.rs`           | `glyphon`-based text layout and rendering for the overlay pill                                |
| `progress_bar.rs`   | Pure layout for the read progress bar under the "Loading…" overlay: three overlay pills       |
| `shader.wgsl`       | Image-quad shader: 2D affine transform, one colour branch per `ShaderMode`                    |
| `overlay.wgsl`      | Rounded-rect pill shader for the title overlay                                                |

## Decision: the backend is pinned per platform, with a wider fallback

`gpu.rs` names the backend each platform asks for (Metal, DX12, Vulkan), and `acquire_gpu` in `renderer.rs` tries it
first and a `PRIMARY | SECONDARY` instance second.

**Why:** left to `Backends::all()`, wgpu registers Vulkan before DX12 and breaks device-type ties by that order, so on
Windows the same GPU comes back as a Vulkan adapter. HDR output is `SurfaceColorSpace::ExtendedSrgbLinear`, a DXGI
swapchain colour space with no Vulkan equivalent, so the flagship feature would have depended on whether a machine
happened to have a Vulkan driver. The fallback is there because a viewer that shows nothing is worse than one without
HDR; taking it logs a `warn` naming what it cost. Each attempt builds its own `Instance` and `Surface`, because the
backend set is fixed at instance creation. Evidence, with line numbers into the wgpu sources:
[docs/notes/gpu-adapter-selection.md](../../../../docs/notes/gpu-adapter-selection.md).

## Decision: `PowerPreference` is `LowPower` on macOS and Linux, `None` on Windows

**Why:** Prvw transforms pixels on the CPU and draws one quad on demand, so the adapter costs almost nothing either way.
What's left is which GPU gets woken and which one is wired to the monitor. `LowPower` keeps a dual-GPU Intel Mac off its
discrete card. On Windows it would be wrong in both directions: `LowPower` takes the integrated GPU on a desktop whose
monitor hangs off a discrete card (a photographer's normal setup, and M2 enumerates HDR outputs per adapter), and
`HighPerformance` wakes a laptop's discrete GPU for a still image. `None` is the only value that skips wgpu's sort,
leaving `IDXGIFactory1::EnumAdapters1` order standing, and that call documents its first entry as the adapter driving
the primary display.

Confirmed by David on 2026-08-27, on the reading of principle 2 in `AGENTS.md`: the app runs for a short time and should
be fast for all of it, so waking the right GPU beats saving power on a viewer that's about to close.

## Decision: every `Surface::configure` goes through `Renderer::configure_surface`

**Why:** since wgpu 30, that call also writes the platform's own colour space onto the surface, from
`SurfaceConfiguration::color_space`. On macOS that means `CAMetalLayer.colorspace`, and wgpu can only name standard
colour spaces. Both of Prvw's answers sit outside that vocabulary: SDR names the _display's ICC profile_ (the pixels
have already been transformed into it, so anything else double-transforms), and EDR names _linear_ Display P3, which
wgpu has no name for. So `configure_surface` writes ours back through `display_profile::restore_layer_colorspace`, and
the renderer holds an `Arc<Window>` and a copy of the display ICC bytes to do it with.

Four call sites would otherwise each have to remember: init, `resize`, `reconfigure_surface_format`, and the
`Lost`/`Outdated` arm inside `render`. That last one is why the app can't do this from outside. A launch reconfigures
three times before the first image is on screen, so forgetting is not a subtle failure, it's the normal one.

## HDR output: what the surface is told, and what the decoder has to write

Three things have to agree, and only one of them is a compile error if it doesn't:

- **The surface format** is `Rgba16Float`, flipped by `reconfigure_surface_format` when `App::want_edr_surface` says so.
- **The colour space** is `SurfaceColorSpace::ExtendedSrgbLinear` (scRGB), named rather than left to `Auto`. On DX12
  that is the only colour space an `Rgba16Float` swapchain presents; on Metal `restore_layer_colorspace` overwrites it
  with linear Display P3.
- **The pixels** come from `color::profiles::HdrDisplaySpace::for_host()`, which picks linear Display P3 on macOS and
  linear BT.709 elsewhere to match. That decision lives with the colour maths, and `color/CLAUDE.md` argues it.

`hdr_surface_supported` asks for the format _and_ the colour space together, because a surface offering one without the
other would take the configure and then clamp everything at display white.

## Decision: potential headroom decides HDR, current headroom only feeds the shader

`Renderer::display_hdr` is the one query, on every platform, through `Surface::display_hdr_info`, and it answers two
questions that must not be conflated (`display_hdr.rs`):

- **`capable`** (can this display show HDR at all?) decides HDR decodes and the EDR surface. macOS: `NSScreen`'s
  `maximumPotentialExtendedDynamicRangeColorComponentValue` above 1.0 (wgpu's `DisplayHeadroom::potential`). Windows:
  `coarse.high_dynamic_range == Some(true)`, which wgpu sets from the output's DXGI colour space, so "HDR mode on".
- **`headroom_now`** (how much brighter than SDR white right now?) is wgpu's `tone_map_headroom()`: `NSScreen`'s live
  `maximumExtendedDynamicRangeColorComponentValue` on macOS, panel peak over SDR white on Windows.

**Why:** the current headroom moves with brightness, ambient light, and battery on macOS, and with the SDR brightness
slider on Windows. Gating decodes on it meant every tweak flushed the cache and re-decoded, and a Mac that happened to
read 1.0 at launch (full brightness) decoded RAWs as SDR until the next display change. `App::adopt_display_hdr` only
clears the cache when `capable` flips. Don't read wgpu's `coarse.high_dynamic_range` on macOS: wgpu derives it from the
_current_ value there.

## The image texture follows storage, the surface follows meaning

`ImagePipeline::upload` picks the texture format from `PixelBuffer` (`Rgba8` → `Rgba8UnormSrgb`, `Rgba16F` →
`Rgba16Float`) and the shader mode from `PixelEncoding::shader_mode()`, while `App::want_edr_surface` reads
`PixelEncoding::is_hdr()`. The two stopped meaning the same thing once 16-bit PNGs and TIFFs started arriving as
`Rgba16F` tagged `DisplaySdr`: those draw on the ordinary `Bgra8UnormSrgb` surface, never the EDR one. Their half-floats
are the display-encoded values passed through the sRGB EOTF (`color::display_sdr`), so the surface's sRGB encode lands
on the same bytes an `Rgba8UnormSrgb` texture would give, rounded to 8 bits once. Nothing in the shader knows the
difference. Branching on storage anywhere downstream is the mistake to avoid.

## One image shader, one mode per encoding, two uniform groups

Every image draws through `ImagePipeline` (`image_pipeline.rs`), which owns the shader, both bind group layouts, the
sampler, and the per-frame uniform, and needs only a device and a queue. `Renderer` holds one and adds the surface.

- **Group 0 is per image**, owned by the image's own bind group: 0 transform, 1 image texture, 2 sampler, 3 gain-map
  texture (a shared 1×1 `Rgba8Unorm` placeholder until M3 gives an encoding a map), 4 `ImageParams` (`mode`, and from M3
  on that mode's parameters). The crossfade rebinds the outgoing `GpuImage` through `prev_uniform_buffer`, so it keeps
  its own texture and mode while the incoming image fades in over it.
- **Group 1 is per frame**: `FrameUniform { headroom_now }`, one buffer for the whole renderer. A headroom change is
  `Renderer::set_headroom_now` plus a redraw; nothing per image is rewritten, which is the whole point of the design.
  `capture_screenshot` binds a second group 1 pinned at 1.0, so the headroom-dependent modes screenshot as their SDR
  rendition.
- **Modes** (`color::hdr::fragment::ShaderMode`, mirrored as constants in `shader.wgsl`): 0 `DisplaySdr` and 1
  `LinearHdrDisplay` are passthrough. M3 adds 2 (gain map) and M4 adds 3 (scene-linear tone map) as `case` arms in the
  shader's `shade`, and the checkerboard composite and the crossfade `fade` stay after the mode's math for every mode.
- **The Rust reference is the spec.** `color::hdr::fragment::shade` is what the WGSL ports, branch for branch.

### Decision: a GPU parity test renders through the real pipeline

**Why:** the WGSL and its Rust reference can drift without any compile error, and the drift (a clip at 1.0, a swapped
matrix) is invisible on an SDR display. The `gpu_parity` tests in `image_pipeline.rs` render a one-row strip through
`ImagePipeline` on a headless device into an `Rgba16Float` target at a forced headroom, read it back, and compare with
`shade` (relative error under 1%, absolute under 1e-3 near black). One row at the target's width puts every fragment on
a texel centre, so the comparison tests the colour math and not the filtering. The module doc says how to add a case.

They run on macOS (Metal) and Windows (DX12, WARP on a GPU-less runner), where a missing adapter fails the test. Linux
CI has no adapter, so they carry `#[cfg_attr(target_os = "linux", ignore = …)]`. ❌ Never turn that into an early return
on a missing adapter: it would pass silently on every platform the day an adapter stops showing up.

## Key patterns

- **Render-on-demand.** `App.needs_redraw` gates frames. Renderer is passive.
- **The overlay pill does outlines too.** `StandalonePill.border_width` above `0.0` makes `overlay.wgsl` subtract the
  same rounded rect shrunk by that width, leaving a ring instead of a fill. That's how the read progress bar draws its 1
  px outline without a second pipeline. `0.0` fills, which is what every other pill wants. A pill whose corner radius is
  half its size is a circle, which is how `tags::overlay` draws its dots.
- **The pill pool has a fixed size.** `OVERLAY_POOL_SIZE` uniform buffers, and a pill past it is silently skipped. Add
  up the busiest frame when a new overlay adds pills; the tag dots alone take up to 14.
- **Two pipelines, two passes.** Image quad renders inside a viewport clipped to the image area (below the title-bar
  strip); the viewport is RESET to the full surface before pills/text.
- **Compositing with vibrancy.** Metal layer is `isOpaque = false`, clear color is `TRANSPARENT`, `zPosition = 1.0` puts
  it in front of AppKit `NSVisualEffectView` subview layers. Opaque image pixels cover the vibrancy; transparent areas
  (title-bar strip) let it show through.

## Title/zoom readout: native when the title bar is on

The title and zoom-% readout are glyphon `TextBlock`s (with a dark pill) **only when the title bar is off** — text
floating over the image, where the pill provides contrast. When the title bar is on, native `NSTextField` labels in the
title-bar area own the readout instead (`window.rs`: `add_titlebar_labels` builds them, `set_titlebar_text` updates them
from the `RedrawRequested` arm). They use the appearance-aware `labelColor` / `secondaryLabelColor`, so they
auto-contrast in light and dark mode — glyphon white-on-glass was unreadable in Light mode. The labels are contentView
subviews composited above the wgpu Metal layer via `zPosition` (a transparent Metal pixel occludes in-window content
behind it, so they can't sit inside the title-bar vibrancy strip), and they're created on both the Liquid Glass and
legacy window paths. `App::titlebar_text` is the single source for both paths' strings. The centered "Loading…" overlay
stays glyphon in both cases. The general rule behind the `zPosition` / both-paths handling — for any native AppKit view
over the Metal layer — is in `platform/macos/CLAUDE.md` ("Native AppKit views over/around the wgpu Metal layer").

## The overlay font is the platform's UI font

Every overlay string (title strip, zoom pill, EXIF panel, histogram labels) is shaped with the host's own UI font, and
`text.rs` resolves which one that is at startup: macOS takes "System Font" (fontdb's English name for `SFNS.ttf`,
registered alongside its localized spellings), Windows asks `SystemParametersInfo(SPI_GETNONCLIENTMETRICS)` for
`lfMessageFont`, and everything else walks a list of what the big Linux desktops ship. Each candidate is checked against
the font database before it's used, the chain falls through to the next when one is missing, and a total miss logs at
`error`.

**Why it can't be one name.** cosmic-text answers a family it can't find by handing back every face in the database
sorted by weight distance, so an unmatched name renders in an arbitrary font rather than failing.
`Family::Name("System Font")` is a macOS-only alias, and that's what the overlay used to ask for everywhere.

**Why Windows doesn't need the bold-alias trick below.** Segoe UI ships separate static weight files, so a bold query
matches a real bold face. Segoe UI Variable would have the same problem `SFNS.ttf` does, but `lfMessageFont` reports
"Segoe UI" on both Windows 10 and 11: the Variable switch is a XAML-layer thing.

**Every `FontSystem` comes from `build_font_system`.** Measurement (wrapped-line counts) and rendering have to shape
with the same faces, or a layout pass disagrees with what gets drawn.

## Gotchas

- **The renderer's `scale_factor` is a copy, and copies go stale.** It's read from the window once at creation, and
  everything logical hangs off it: `logical_width` / `logical_height` (what the zoom math measures the window with) and
  the size the overlay text is rasterised at. A window that moves to a display with a different factor - a Retina Mac to
  a 1x external monitor, a 150% Windows laptop panel to a 100% desktop screen - has to be told, which `app.rs` does from
  the `ScaleFactorChanged` arm via `set_scale_factor`. Anything else that caches the factor owes the same.
- **Image texture must be explicitly destroyed on replace.** `set_image` holds the previous image in `Renderer.image`
  and calls `GpuImage::destroy()` before allocating the new one (a crossfade's outgoing image is destroyed by
  `end_crossfade`). M3's gain-map texture belongs in `GpuImage` so it's destroyed alongside. Without this, Metal keeps
  the old unified-memory backing resident. A long navigation session through 20 MP RAWs can grow RSS by gigabytes even
  though `bind_group` was replaced.
- **Screenshot path differs from main render.** `capture_screenshot` strips the viewport offset, pills, and text. Pixel
  tests of the live window's appearance need a different approach. For QA work that needs the full window (overlays,
  title bar, window chrome), use the debug-only `screenshot_window` MCP tool in `qa/window_capture.rs`, which
  photographs the real window on macOS and Windows.
- **Surface format is `Bgra8UnormSrgb` on macOS in the SDR path.** Screenshot readback swizzles BGRA → RGBA before PNG
  encoding. Phase 5.1: when the current image is an HDR RAW and the display reports EDR headroom, the surface flips to
  `Rgba16Float` and the CAMetalLayer EDR properties go on. Screenshots still render through an SDR offscreen target
  (clamped to display-white) so PNG readback stays format-agnostic.
- **HDR surface transitions rebuild three pipelines.** `reconfigure_surface_format` rebuilds the image-quad pipeline,
  the overlay pipeline, and the glyphon text renderer against the new format. Shader modules and pipeline layouts are
  cached, so only the `RenderPipeline` objects get recreated. The glyphon renderer is rebuilt wholesale because
  `TextAtlas::new` pins the format. Cheap: one allocation plus a fresh swash cache. See
  `docs/notes/raw-support-phase5.md`.
- **`CAMetalLayer` is a sublayer, not the NSView's direct layer.** Walk `[ns_view layer].sublayers`. See
  `crate::color::display_profile::set_layer_colorspace`.
- **wgpu 30 API quirks.** `Instance::new()` takes a value. `get_current_texture()` returns an enum. Presenting is
  `Queue::present(texture)`, not `SurfaceTexture::present()`. `Buffer::get_mapped_range` returns a `Result`.
  `PipelineLayoutDescriptor` uses `immediate_size`. `RenderPassColorAttachment` requires `depth_slice`.
- **Ask the instance for nothing you don't draw with.** `InstanceFlags::default()` carries `VALIDATION_INDIRECT_CALL` in
  **release** builds too, and that flag makes `request_device` compile a compute shader that range-checks indirect draw
  arguments. Prvw issues no indirect calls, so it guarded nothing — and on macOS 13 Apple's Metal compiler fails that
  shader with an internal error, which fails device creation and crashed every v0.16.0 user the moment a window wanted a
  GPU. `instance_flags()` subtracts the bit, and a unit test keeps it subtracted. The general rule: an unused wgpu
  feature is not inert, because wgpu may build GPU resources for it at device creation, on a driver you have never seen.
- **`Surface::display_hdr_info` is main-thread-only on Metal.** It reads `NSScreen` through the hosting `NSWindow`, and
  answers "nothing known" off the main thread rather than failing. Call it from the event loop.
- **Shaders are `include_str!`'d** relative to `renderer.rs`. Keep them colocated.
