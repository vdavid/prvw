# Render (GPU, view math, overlays)

Everything that turns app state into pixels on screen.

| File           | Purpose                                                                                       |
| -------------- | --------------------------------------------------------------------------------------------- |
| `renderer.rs`  | `wgpu` instance/device/surface, two pipelines (image quad, overlay pill), texture upload, screenshot readback |
| `view.rs`      | `ViewState`: zoom/pan state, `fit_zoom`, cursor-centered zoom, `TransformUniform` for the vertex shader |
| `text.rs`      | `glyphon`-based text layout and rendering for the overlay pill                                |
| `shader.wgsl`  | Image-quad vertex/fragment shader with a 2D affine transform                                   |
| `overlay.wgsl` | Rounded-rect pill shader for the title overlay                                                |

## Key patterns

- **Render-on-demand.** The renderer is passive — it only redraws when `App.needs_redraw`
  flips true. No continuous loop. GPU idle when there's nothing to show.
- **Transform via vertex uniform.** Zoom and pan are a 2D affine transform applied in
  `shader.wgsl`'s vertex stage. No re-decode, no re-upload — just a uniform write.
- **Two pipelines, two passes.** Image quad renders first inside a viewport clipped to
  the image area (`set_viewport(0, offset_px, sw, sh - offset_px)`). The viewport is
  RESET to the full surface before pills/text so the title overlay floats above.
- **`set_viewport` remaps NDC.** Not just a scissor — `[-1,1]` is mapped to the viewport
  rect. The transform's denominator must be `effective_height = sh - offset` so
  `sy=1.0` exactly fills the clipped viewport.
- **Compositing with vibrancy.** Metal layer is `isOpaque = false`, clear color is
  `TRANSPARENT`, and `zPosition = 1.0` pushes it in front of the AppKit
  `NSVisualEffectView` subviews. Opaque image pixels cover the vibrancy; transparent
  areas (title bar strip) let it show through.

## Gotchas

- **Screenshot path differs from the main render** (`capture_screenshot`). Strips
  viewport offset, pills, and text. Screenshot tests can't verify the title-bar-on
  state's viewport clipping. To make screenshots match the window, factor out a shared
  inner-render function that both paths call.
- **Surface format is `Bgra8UnormSrgb` on macOS.** The screenshot readback reads raw
  BGRA and swizzles to RGBA before PNG encoding. Change the surface format = update
  the swizzle.
- **`CAMetalLayer` is a sublayer**, not the NSView's direct layer. `set_layer_colorspace`
  checks `respondsToSelector:setColorspace:` and walks sublayers. Without this,
  `msg_send![layer, setColorspace:]` panics inside winit's ObjC event loop, which
  aborts (panics can't unwind through `extern "C"`).
- **wgpu 29 API quirks.** `Instance::new()` takes a value (not reference).
  `get_current_texture()` returns an enum, not `Result`. `PipelineLayoutDescriptor` uses
  `immediate_size` instead of `push_constant_ranges`. `RenderPassColorAttachment`
  requires `depth_slice`.
- **`shader.wgsl` + `overlay.wgsl` are `include_str!`'d** relative to `renderer.rs`, so
  they must stay colocated inside `render/`.
