# Render (infrastructure — wgpu)

Not a feature — this is the GPU rendering scaffolding. Features like `zoom` (which
owns `ViewState`) plug transforms into the renderer's uniform buffer via
`crate::zoom::view::TransformUniform`.

| File           | Purpose                                                                              |
| -------------- | ------------------------------------------------------------------------------------ |
| `renderer.rs`  | `wgpu` instance/device/surface, two pipelines (image quad, overlay pill), screenshot readback |
| `text.rs`      | `glyphon`-based text layout and rendering for the overlay pill                        |
| `shader.wgsl`  | Image-quad vertex/fragment shader with a 2D affine transform                          |
| `overlay.wgsl` | Rounded-rect pill shader for the title overlay                                        |

## Key patterns

- **Render-on-demand.** `App.needs_redraw` gates frames. Renderer is passive.
- **Two pipelines, two passes.** Image quad renders inside a viewport clipped to the
  image area (below the title-bar strip); the viewport is RESET to the full surface
  before pills/text.
- **Compositing with vibrancy.** Metal layer is `isOpaque = false`, clear color is
  `TRANSPARENT`, `zPosition = 1.0` puts it in front of AppKit `NSVisualEffectView`
  subview layers. Opaque image pixels cover the vibrancy; transparent areas
  (title-bar strip) let it show through.

## Gotchas

- **Screenshot path differs from main render.** `capture_screenshot` strips the
  viewport offset, pills, and text. Pixel tests of the live window's appearance need
  a different approach.
- **Surface format is `Bgra8UnormSrgb` on macOS.** Screenshot readback swizzles
  BGRA → RGBA before PNG encoding. Phase 5.0 uploads `Rgba16Float` textures for HDR
  RAWs but the surface itself stays `Bgra8UnormSrgb`; values above 1.0 survive the
  texture sample but quantise back to SDR at the final blend. The surface-format
  switch + `CAMetalLayer.wantsExtendedDynamicRangeContent` land in Phase 5.1 (see
  `docs/notes/raw-support-phase5.md`).
- **`CAMetalLayer` is a sublayer, not the NSView's direct layer.** Walk
  `[ns_view layer].sublayers`. See `crate::color::display_profile::set_layer_colorspace`.
- **wgpu 29 API quirks.** `Instance::new()` takes a value. `get_current_texture()`
  returns an enum. `PipelineLayoutDescriptor` uses `immediate_size`.
  `RenderPassColorAttachment` requires `depth_slice`.
- **Shaders are `include_str!`'d** relative to `renderer.rs` — keep them colocated.
