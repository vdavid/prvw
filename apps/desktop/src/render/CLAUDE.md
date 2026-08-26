# Render (infrastructure: wgpu)

Not a feature. This is the GPU rendering scaffolding. Features like `zoom` (which owns `ViewState`) plug transforms into
the renderer's uniform buffer via `crate::zoom::view::TransformUniform`.

| File           | Purpose                                                                                       |
| -------------- | --------------------------------------------------------------------------------------------- |
| `gpu.rs`       | Which backend and which adapter this platform asks wgpu for, as a pure policy                 |
| `renderer.rs`  | `wgpu` instance/device/surface, two pipelines (image quad, overlay pill), screenshot readback |
| `text.rs`      | `glyphon`-based text layout and rendering for the overlay pill                                |
| `shader.wgsl`  | Image-quad vertex/fragment shader with a 2D affine transform                                  |
| `overlay.wgsl` | Rounded-rect pill shader for the title overlay                                                |

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

## Key patterns

- **Render-on-demand.** `App.needs_redraw` gates frames. Renderer is passive.
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
- **Image texture must be explicitly destroyed on replace.** `set_image` holds the previous `wgpu::Texture` in
  `Renderer.image_texture` and calls `texture.destroy()` before allocating the new one. Without this, Metal keeps the
  old unified-memory backing resident. A long navigation session through 20 MP RAWs can grow RSS by gigabytes even
  though `bind_group` was replaced.
- **Screenshot path differs from main render.** `capture_screenshot` strips the viewport offset, pills, and text. Pixel
  tests of the live window's appearance need a different approach. For QA work that needs the full window (overlays,
  title bar, vibrancy), use the debug-only `screenshot_window` MCP tool, which shells out to `screencapture -l`.
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
- **wgpu 29 API quirks.** `Instance::new()` takes a value. `get_current_texture()` returns an enum.
  `PipelineLayoutDescriptor` uses `immediate_size`. `RenderPassColorAttachment` requires `depth_slice`.
- **Shaders are `include_str!`'d** relative to `renderer.rs`. Keep them colocated.
