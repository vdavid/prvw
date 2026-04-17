# Zoom & pan

`ViewState` (zoom + pan math, `TransformUniform` for the shader), the zoom-related
`AppCommand` effects, `zoom::State` (feature-owned fields on `App`), and the
Settings → Zoom panel.

| File                | Purpose                                                                                 |
| ------------------- | --------------------------------------------------------------------------------------- |
| `mod.rs`            | `zoom::State { auto_fit, enlarge, scroll_to_zoom, view: ViewState }` + `from_settings`  |
| `view.rs`           | `ViewState`: zoom/pan state, `fit_zoom`, cursor-centered zoom, min_zoom floor           |
| `settings_panel.rs` | Settings → Zoom panel: Auto-fit window + Enlarge small images                           |

## State

`App.zoom: zoom::State` owns this feature's fields:
- `auto_fit` — setting, whether window resizes to match each image
- `enlarge` — setting, whether to upscale small images
- `scroll_to_zoom` — setting, scroll wheel zooms vs navigates
- `view: ViewState` — runtime zoom/pan math + `TransformUniform`

Inside `ViewState`: `zoom: f32` is the absolute scale (1.0 = pixel-perfect),
`pan_x/pan_y`, `min_zoom` (the floor), image + window dimensions.

## Zoom model

Zoom is **absolute**: `zoom=1.0` means 1 image pixel = 1 screen pixel. `fit_zoom()`
is the zoom that exactly fills the content area (< 1.0 for large images, > 1.0 for
small ones). `min_zoom` is the floor — prevents zooming out past fit.

On image load, `App::apply_initial_zoom` picks the starting zoom and floor based on
the three settings (`auto_fit`, `enlarge`, `min_zoom`) and the image vs window sizes.
See the full matrix in `apps/desktop/CLAUDE.md`.

## Transform

Zoom and pan become a 2D affine transform in the vertex shader via `TransformUniform`
(a uniform buffer write). No re-decode, no re-upload — just a uniform update.

## Gotchas

- **Mouse-Y for zoom-at-cursor must subtract `content_offset_y`.** The title-bar
  strip shifts the image area down; the cursor position in window coords doesn't
  know about that.
- **`set_viewport` remaps NDC, not just scissor.** Transform's denominator is
  `effective_height = surface_h - offset` so `sy=1.0` fills the viewport rect.
