# Thumbnails (macOS-only)

Background-generate thumbnails for every file in the current folder so navigating
to an image outside the full-decode preload window shows a blurry placeholder
instantly instead of a blank screen. Relies on macOS's system-wide QuickLook
thumbnail cache (`quicklookd`), shared with Finder, Preview, and every other
Mac app — no disk storage of our own.

| File           | Purpose                                                                  |
| -------------- | ------------------------------------------------------------------------ |
| `mod.rs`       | `State { scheduler, cache, source_dims, paths, requests }` + public API  |
| `scheduler.rs` | Pure state machine: queue ordering, parallelism cap, pause/resume        |
| `metadata.rs`  | ImageIO `CGImageSource`-based pixel-dimension reader (no decode)         |
| `quicklook.rs` | `QLThumbnailGenerator` bridge + `CGImage → RGBA8` blit via `CGBitmapContext` |

## Flow

1. `App::resumed` calls `State::set_folder(paths, current)` after the directory scan.
2. The scheduler enqueues every folder index, ordered centered-outward but with
   indices inside the full-decode preload window (`|i − current| ≤ 2`) pushed
   last — the full-decode preloader will cover those anyway, so they're the
   lowest-value thumbs to fetch.
3. `App::pump_thumbnail_requests` drains the scheduler up to `max_parallel`
   (`available_parallelism() / 2`, min 1) and submits each to `QLThumbnailGenerator`.
4. `quicklookd` generates (or cache-hits) a 512 × scale thumb and calls our
   completion block on its internal queue.
5. The block converts the `CGImage` to RGBA8 and fires
   `AppCommand::ThumbnailReady { index, rgba, width, height }` via
   `EventLoopProxy`, which `winit` delivers as a `user_event` on the main thread.
6. `App::execute_command` stores the thumb in the cache and — if this thumb is
   for `pending_current` — uploads it into the image texture as a placeholder.
   The full decode later replaces it.

## Key patterns

- **No dedicated thread.** `QLThumbnailGenerator` is async inside the system
  (`quicklookd` runs out-of-process). Main thread submits; completion blocks
  forward results as winit user events. Wrapping this in a worker thread
  would just proxy through an already-async API.
- **System cache, not ours.** `quicklookd` maintains an on-disk cache keyed by
  file URL + mtime. Modified files auto-invalidate, which means we never
  check staleness ourselves. Disk cost: zero; cache hygiene: OS-managed.
- **Raw FFI for CF / CG / ImageIO.** `objc2-*` 0.3 ships bindings for a lot
  but not `CGBitmapContextCreate` (only the new adaptive variant). The few
  calls we need are declared in `extern "C"` blocks locally, matching the
  pattern in `color::display_profile`.
- **ImageIO dims before thumb pixels.** `apply_thumbnail_auto_fit` runs on
  cache-miss navigation to resize the window to the final image size
  *before* any pixels paint. The thumb (and later the full decode) then fill
  the already-correct window. No second resize when the full decode lands —
  the numbers match.
- **Thumb routes through the same display pipeline as full images.**
  `display_thumbnail_placeholder` calls `App::prepare_display(source_w,
  source_h, false)` → `renderer.set_image(thumb)` → `App::finalize_display()`,
  exactly like `display_from_cache` does for full images. The shared
  helpers handle window auto-fit, EDR surface state, and `apply_initial_zoom`.
  Without this, thumbs landed at whatever zoom the previous image left
  behind (often 1:1, looking like a crop). Linear sampling on upscale
  provides a soft, blurred appearance that signals "not final." A
  dedicated blur shader is not currently implemented — the linear-sampler
  softness is sufficient.

## Pause semantics

While a primary decode is pending (`navigation.pending_current.is_some()`),
the scheduler is paused so it doesn't compete for I/O or shared system CPU.
Resumed on decode completion (success or failure). Already-in-flight requests
keep running — cancellation has I/O cost and `quicklookd` is usually near-done.

## Thumbnail size

`CGSize { 512, 512 }` at `NSScreen.backingScaleFactor` (2.0 Retina → 1024 effective
pixels). This matches QuickLook's gallery cache bucket, so folders the user has
browsed in Finder's gallery view hit the cache instantly. Above 1024 effective,
`quicklookd` renders from source every time — falls off the cache entirely.

## MCP

Exposed via the `thumbnails_status` MCP tool. Returns folder length, current
index, in-flight indices, queue length, cached indices, failed indices, paused
flag, and parallelism cap.

## Gotchas

- **`QLThumbnailRepresentation::CGImage()` returns a `Retained<CGImage>` wrapper.**
  Pass its raw pointer via `Retained::as_ptr` to the FFI `CGContextDrawImage`;
  the `Retained` drops at end-of-scope and releases naturally.
- **Completion blocks run on quicklookd's queue.** `EventLoopProxy` is `Send`,
  so forwarding is fine, but don't touch `App` fields inside the block — the
  block exists only to post a user event. All mutation happens on the main
  thread.
- **Cancellation semantics.** We don't cancel individual in-flight requests
  mid-flight. On folder change, `RequestTable::cancel_all` fires `cancelRequest`
  on every live request. Individual cancellation isn't wired because the
  scheduler's queue-based model covers the common case.

## Future work

- **File watcher integration.** `State::set_folder` is the single entry point
  for the path list. When `fsevents` watching lands, it just calls `set_folder`
  again with the updated list — scheduler cancels orphaned requests and
  enqueues new ones.
- **Dedicated blur shader.** Current: linear-sampler softening on upscale.
  A separable Gaussian in a fragment shader would look nicer behind the
  "Loading..." overlay. Not required for correctness.
