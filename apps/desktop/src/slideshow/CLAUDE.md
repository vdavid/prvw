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

**The dwell starts when an image is actually displayed, not when it was requested.** `display_from_cache` sets
`next_advance = now + interval` (when running). So a slow decode never eats into or skips the next slide's time — each
image gets its full interval once it's on screen. This also means manual navigation resets the dwell for free (it goes
through the same display path), so there's no separate timer-bump on the nav commands.

**Advance is gated on readiness** (`slideshow_ready_to_advance`): the current image must be fully shown
(`pending_current.is_none()`, i.e. not a "Loading…" placeholder) AND the next image must already be decoded
(`image_cache.contains`). If not, `about_to_wait` holds on the current image; a preloader-completion event
(`PreloaderProgress`) wakes it to re-check, so the switch is always instant and clean even when a big image decodes
slower than the per-image interval. A `slideshow::MAX_HOLD` (20 s) grace cap bounds the hold so a corrupt or
never-decoding next image can't stall the show forever. When neighbor preloading is off (benchmark setting), the
next-cached requirement is skipped. While holding, `schedule_wakeup` waits on `deadline + MAX_HOLD` rather than the
already-passed deadline, so it never busy-spins on `WaitUntil(past)`.

`[` / `]` (and Slideshow → Increase/Decrease speed) call `adjust_slideshow_speed`, stepping `seconds` by one within
`MIN_SECONDS..=MAX_SECONDS`. They adjust the setting whether or not a slideshow is running. Bare `S` in image mode
(everywhere), ⌘S (macOS menu accelerator), and Slideshow → Start/Stop all toggle `running`; the menu item's label flips
between "Start slideshow" and "Stop slideshow". `S` is bound in image mode only, because browse mode's tree and list own
bare letters for type-ahead select. `SharedAppState::slideshow_running` mirrors the flag for the QA server.

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

If the advance target isn't cached yet, `after_position_change` clears `pending_crossfade`. Otherwise the preview
placeholder shown while decoding would become the "outgoing" frame, and an advance that stalls on a decode isn't a
smooth transition anyway. Forward slideshow advances are almost always preloaded, so this rarely triggers.

## Loop vs. `loop_navigation`

Slideshow looping (`Settings::slideshow_loop`, default on) is independent of `navigation::loop_navigation` (which
governs manual Next/Previous wrapping). At the last image, `slideshow_advance` wraps to the first when slideshow looping
is on, otherwise stops the slideshow.
