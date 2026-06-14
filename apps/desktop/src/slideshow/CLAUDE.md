# Slideshow

Auto-advance through the directory on a timer, with an optional crossfade between images. Deliberately thin: it owns no
navigation logic. When the dwell timer fires, `App::slideshow_advance` reuses the normal `navigate_by` /
`navigate_to_first` paths.

| File                | Purpose                                                                                                                                                          |
| ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `mod.rs`            | `slideshow::State { running, seconds, crossfade_enabled, loop_enabled, next_advance, crossfade }` + constants + `stepped_seconds` / `clamp_seconds` pure helpers |
| `settings_panel.rs` | Settings → Slideshow panel (macOS): time-per-image slider (1–30 s) + Crossfade + Loop toggles, own `SlideshowDelegate`                                           |

## State

`App.slideshow: slideshow::State` holds the runtime. `seconds` / `crossfade_enabled` / `loop_enabled` mirror the
`Settings::slideshow_*` fields and persist on change. `next_advance` is the `Instant` the timer fires (`Some` only while
`running`); `crossfade` is the start `Instant` of an in-flight fade (`None` otherwise).

## Timing (no continuous render loop)

The dwell timer rides the same `ControlFlow::WaitUntil` mechanism as the nav debounce. `App::schedule_wakeup` (called at
the end of `about_to_wait`) picks the earliest of {nav deadline, `next_advance`, next crossfade frame} and sets one
`WaitUntil`; with nothing pending it falls back to `Wait`. So the app still sleeps between slides — it doesn't spin.
`about_to_wait` fires `slideshow_advance` once the deadline passes.

Manual navigation while running calls `slideshow_bump_timer` (in the executor's nav arms), pushing `next_advance` out a
full interval so the slide you just chose gets its full dwell time.

`[` / `]` (and Slideshow → Increase/Decrease speed) call `adjust_slideshow_speed`, stepping `seconds` by one within
`MIN_SECONDS..=MAX_SECONDS`. They adjust the setting whether or not a slideshow is running. ⌘S (Slideshow → Start/Stop)
toggles `running`; the menu item's label flips between "Start slideshow" and "Stop slideshow".

Starting a slideshow does NOT enter fullscreen, jump to image 1, or hide overlays — it only arms the timer.

## Crossfade

A 300 ms (`CROSSFADE_DURATION`) two-texture blend, slideshow-advance only — manual navigation always cuts. Driven
frame-by-frame from `App::drive_crossfade`, which ramps a fade factor and requests redraws (~60 fps via the 16 ms
`WaitUntil`), then drops the outgoing texture when done. The fade factor travels to the GPU in `TransformUniform`'s
spare `col1.z` slot (1.0 = opaque everywhere except mid-fade); the image fragment shader multiplies output alpha by it,
and the image pipeline uses alpha blending (not REPLACE) so the incoming image composites over the outgoing one. See
`crate::render::renderer` `begin_crossfade` / `set_crossfade` / `end_crossfade`.

### Decision/Why: crossfade only when the surface size is unchanged

`display_from_cache` starts a crossfade only if the surface size is identical before and after `prepare_display`. When
auto-fit resizes the window for a differently-sized image, the outgoing image's saved transform would be stale against
the new surface, so we cut instead. The common case — a folder of same-orientation camera shots — crossfades cleanly;
orientation changes cut. Recomputing the outgoing transform for the new size (to crossfade across a resize) is possible
but wasn't worth the complexity.

### Decision/Why: no crossfade on a cache miss

If the advance target isn't cached yet, `after_position_change` clears `pending_crossfade`. Otherwise the thumbnail
placeholder shown while decoding would become the "outgoing" frame, and an advance that stalls on a decode isn't a
smooth transition anyway. Forward slideshow advances are almost always preloaded, so this rarely triggers.

## Loop vs. `loop_navigation`

Slideshow looping (`Settings::slideshow_loop`, default on) is independent of `navigation::loop_navigation` (which
governs manual Next/Previous wrapping). At the last image, `slideshow_advance` wraps to the first when slideshow looping
is on, otherwise stops the slideshow.
