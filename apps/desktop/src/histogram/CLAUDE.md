# Histogram

256-bin RGB histogram overlay anchored to the top-right of the window. Toggled via View → Histogram or the bare `H` key.

| File         | Purpose                                                                                                                         |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------- |
| `mod.rs`     | `histogram::State { visible, data, hover_bin }` + `HistogramRect` (cursor-to-bin mapping)                                       |
| `compute.rs` | `compute(&PixelBuffer, &PixelEncoding)` — 256-bin RGB scan; rayon-parallel above 4 MP; RGBA8, and RGBA16F by encoding (below)   |
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

## Decision: half-floats bin by what they mean, not how they're stored

**Decision:** `compute` takes the `PixelEncoding` alongside the buffer. `DisplaySdr` half-floats (16-bit PNGs and TIFFs)
go back through the sRGB OETF (`color::display_sdr::display_sdr_f16_to_u8`, a 64K lookup) and land in exactly the bin
their 8-bit equivalent would. `LinearHdrDisplay` (RAW HDR) and `SceneLinear` clamp at 1.0 and quantize linearly, so
above-white highlights read as clipped.

**Why:** a `DisplaySdr` half-float is the linearized display value. Binning it as-is skews the whole histogram dark
(mid-grey lands near bin 55). `display_sdr_half_floats_bin_like_the_bytes_they_stand_for` is the anchor.

A `GainMapped` image (gain-map JPEG on an HDR display) bins its SDR base, the RGBA8 bytes, which are in Display P3
rather than the display's profile. The highlights its gain map adds at the current headroom don't show: they exist only
in the shader, and change with the brightness. **Decision: kept in P3.** On a Display P3 panel (every Mac with an HDR
display) P3 bytes are the display's bytes, so nothing differs where gain maps show at all; elsewhere the bins shift by
the P3-to-display hue difference, and converting a copy of the base only for the histogram would cost a full transform
per image.

A `SceneLinear` image (a PQ or HLG PNG or an HDR float TIFF on an HDR display) bins its stored values, linear light in
the source's primaries before the tone map, clamped at SDR white like RAW HDR. **Known simplification:** that skews dark
(no transfer curve) and piles every highlight into bin 255, and it's not what's on screen, which the shader tone-maps at
the live headroom. The spec allows it for now; the SDR rendition the same file gets on an SDR display bins correctly,
because it's RGBA8.

## Decision: `plot_rect_for` is the single layout source of truth

**Decision:** `overlay::plot_rect_for(window_width, content_offset_y) -> HistogramRect` returns the same plot rect that
`overlay::build` draws into. Both `build` (for rendering) and `App::update_histogram_hover` (for cursor → bin mapping)
call it.

**Why:** The hover handler used to read `histogram::State.last_rect`, which was only assigned after a successful
`Renderer::render` call. An MCP `set_cursor_position` arriving before the first frame produced no hover bin, masked in
tests by a `thread::sleep`. Computing the rect deterministically removes the timing dependency and the test sleep.
