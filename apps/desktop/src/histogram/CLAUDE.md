# Histogram

256-bin RGB histogram overlay anchored to the top-right of the window. Toggled via View → Histogram or the bare `H` key.

| File         | Purpose                                                                                                                         |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------- |
| `mod.rs`     | `histogram::State { visible, data, hover_bin }` + `HistogramRect` (cursor-to-bin mapping)                                       |
| `compute.rs` | `compute(&PixelBuffer)` — 256-bin RGB scan; rayon-parallel above 4 MP; supports RGBA8 and RGBA16F (HDR clamps to 1)             |
| `overlay.rs` | Visual layout: backdrop pill, axis ticks, bar plot draw call, hover readout. `plot_rect_for(...)` is the layout source of truth |

The GPU-side bar rendering lives in `crate::render::renderer` (search for `HistogramDrawCall`) and the WGSL is at
`render/histogram.wgsl`.

## Decision: lazy compute, gated on visibility

**Decision:** `histogram::compute::compute` runs only when `state.visible` is true. Off-by-default users pay zero
compute cost per nav. When the user toggles the panel on (`AppCommand::ToggleHistogram` in `app/executor.rs`) and
`state.data.is_none()`, we compute lazily from the cached `DecodedImage`. New images recompute via `display_from_cache`
/ `display_decoded_direct`, but only when visible.

**Why:** The compute is a 256-bin scan over every pixel. A 24 MP RAW costs a few ms on the main thread, every
navigation. Most users never enable the panel, so doing the work eagerly is pure waste.

## Decision: `plot_rect_for` is the single layout source of truth

**Decision:** `overlay::plot_rect_for(window_width, content_offset_y) -> HistogramRect` returns the same plot rect that
`overlay::build` draws into. Both `build` (for rendering) and `App::update_histogram_hover` (for cursor → bin mapping)
call it.

**Why:** The hover handler used to read `histogram::State.last_rect`, which was only assigned after a successful
`Renderer::render` call. An MCP `set_cursor_position` arriving before the first frame produced no hover bin, masked in
tests by a `thread::sleep`. Computing the rect deterministically removes the timing dependency and the test sleep.
