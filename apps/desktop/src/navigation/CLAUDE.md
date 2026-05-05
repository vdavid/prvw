# Navigation

Scan the parent directory for images, preload adjacent files in the background, and keep an LRU cache budgeted at 512 MB
(SDR) or 1 GB (HDR, Phase 5). The cache auto-scales when the RAW pipeline's `hdr_output` flag flips or the display's EDR
headroom crosses the 1.0 boundary, so preload count stays constant as we double per-pixel bytes for RAW RGBA16F.

| File           | Purpose                                                                                                                                                                                                                                                |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `mod.rs`       | `navigation::State { dir_list, preloader, image_cache, history, current_image_size, preload_neighbors, pending_current, last_direction, pending_nav_delta, nav_deadline, loop_navigation }`; `format_offset` + `format_bytes` + `NAV_DEBOUNCE` helpers |
| `directory.rs` | `DirectoryList`: scan parent dir for supported extensions, sort, track current position; `Direction`-aware `preload_range(count, dir, loop_on)`; `go_by(delta, loop_on)`; absolute jumps via `go_to_first()` / `go_to_last()`                          |
| `preloader.rs` | Serial `std::thread` worker + `ImageCache` with LRU + retain-only eviction (512 MB / 1 GB budget)                                                                                                                                                      |
| `wrap.rs`      | Pure-logic loop helpers: `active_preload_indices(current, total, radius, loop_on)`, `step_next` / `step_previous`. Used by `App::refresh_preload_window` on loop toggle / sort change and by `navigate_by` for cache `keep` set                        |
| `sort.rs`      | `SortBy { Name, Date, FileType }` (all ascending) + `sort_files()` comparator. Name uses natural alphanumeric (`photo_2 < photo_10`), case-insensitive. Date and FileType fall back to Name as tiebreaker                                              |

## State

`App.navigation: navigation::State` owns this feature's runtime. Note the `history` field holds
`VecDeque<NavigationRecord>`. The type is defined in `crate::diagnostics` (it's a measurement record). Navigation pushes
entries; diagnostics formats them.

## Navigation render path

On cache hit, `navigate_by` renders from cache synchronously and submits neighbor preloads via
`Preloader::request_neighbor_preload`. On cache miss it sets `State.pending_current = Some(index)`, shows a "Loading…"
title, and calls `Preloader::prioritize_target(index, path, total)` — which cancels every other in-flight task so the
priority-0 target gets the worker's full attention. `poll_preloader` runs the render when
`PreloadResponse::Ready { index }` matches `pending_current`, then queues the now-displayed image's neighbors (deferred
until after the target arrives — see "Preloader prioritization" below). The main thread never decodes navigation targets
directly. Only settings re-decode and `Refresh` still call the sync `display_image` path.

## Debounced navigation

User input (arrow keys, mouse wheel, Next/Previous menu items) goes through `AppCommand::NavigateDebounced`, which
accumulates a signed delta in `State.pending_nav_delta` and sets `State.nav_deadline` to now + `NAV_DEBOUNCE` (30 ms).
`App::about_to_wait` fires the flush when the deadline elapses; winit gets `ControlFlow::WaitUntil(deadline)` so the
wake is precise. A sustained wheel spin collapses into a single `navigate_by(±20)` jump with one decode, not twenty. QA
/ MCP / HTTP use the immediate `AppCommand::Navigate` path, which flushes pending first so automated tests see
deterministic state.

## Key patterns

- **Dedicated `std::thread` worker, not a rayon pool.** Tasks are queued through an `mpsc::channel` to a single OS
  thread that pops and runs them serially. Responses come back via another `mpsc::channel`. An in-flight
  `HashMap<index, Arc<AtomicBool>>` lets us cancel only the tokens for indices that dropped out of the priority list;
  tasks still wanted keep their existing token.

  **Why not rayon?** rawler's internal `par_iter` inherits the caller's rayon pool. On a 1-thread custom pool, rawler's
  parallel stages (demosaic, chroma_nr, sharpen) collapse to 1 thread and balloon ~10×. A plain OS thread isn't a rayon
  worker, so `par_iter` inside it falls back to the global pool (every logical core), matching the main-thread sync
  decode path. See the comment block above `Preloader` in `preloader.rs` for the measurement table.

- **Direction-aware priority.** `DirectoryList::preload_range` takes a `Direction` (forward / backward / unknown) and
  returns indices ordered by likelihood of being viewed next. Forward nav returns `[N+1, N+2, N-1, N-2]`. Used by
  `submit_neighbor_preload` to pick which neighbors to warm.
- **Cancellation.** Preload tasks hold an `Arc<AtomicBool>`; the preloader flips tokens via `prioritize_target`
  (cancel-all-except-target) or `request_neighbor_preload` (cancel only those that drop out of the new list). Cancelled
  task closures still get pulled off the FIFO channel but exit fast at `load_image`'s cancellation check.
- **Supported extensions are decided by `decoding`.** `DirectoryList` filters via `decoding::is_supported_extension`.
  New format support = one change, two effects (decode + list).
- **Preload can be disabled for benchmarking.** `State.preload_neighbors` (driven by Settings → General → "Preload
  next/prev images", default on) gates both preload call sites in `app.rs`. When off, only the currently-displayed image
  consumes decode work. Intended for single-image cold-start perf measurements where concurrent preloads would skew the
  per-stage timings logged by `decoding::raw::decode`.

## Preloader prioritization (rapid-nav UX)

The preloader has two distinct submission methods:

- `prioritize_target(target, path, total)` — used on cache-miss navigation. Cancels every other in-flight task, then
  queues `target` if not already in flight. Trade-off: neighbors that were alive get cancelled and may need re-decode
  later; in exchange, the user-visible target gets the worker's full attention immediately.

- `request_neighbor_preload(tasks, current_index, total)` — used to warm the cache around an already-displayed image
  (cache-hit nav, post-arrival warm-up). Cancels only indices that dropped out of the new requested set. Doesn't fight
  the priority-0 target.

**Neighbor preload is deferred on cache miss.** `navigate_by` submits _only_ the priority-0 target via
`prioritize_target`. Neighbors are queued from `poll_preloader` after `PreloadResponse::Ready` arrives for
`pending_current` and `display_from_cache` has run. This keeps the FIFO channel small during rapid navigation: a 5-nav
burst queues 5 priority-0 closures (4 cancelled, 1 alive), not 5 × 5 = 25 closures all competing for the worker.

## Gotcha/Why: winit `ControlFlow::Wait` ↔ preloader response channel

`poll_preloader` only runs from `App::about_to_wait` and `App::window_event`. When winit is in `ControlFlow::Wait`, the
main thread sleeps until an OS event arrives. The preloader's `mpsc::channel` does **not** wake winit by itself, so a
freshly-decoded image's `PreloadResponse::Ready` can sit in the channel for _seconds_ — until the user moves the mouse,
presses a key, or some other OS event nudges the loop.

**Fix:** the preloader worker thread sends `AppCommand::PreloaderProgress` via `EventLoopProxy::send_event` after every
response. The handler is a no-op — the wake itself is the side effect, because winit always runs `about_to_wait` after
any user event, which is where we drain the channel. Same pattern the thumbnail completion path uses.

This was masked when neighbor preload was always-on (constant decode activity kept the loop awake) and surfaced only
after the deferred-neighbor change reduced background work. If you add another async-result path, include the same
`send_event` wakeup or the result will be silently delayed.

## Thumbnail placeholder (macOS)

On a cache-miss navigation the title bar shows "Loading…" and a centered "Loading..." pill appears mid-screen. If the
`thumbnails` module has a cached thumb for the target index, that thumb is uploaded to the image texture as a blurry
placeholder, and `apply_thumbnail_auto_fit` resizes the window to the source dimensions (read via ImageIO, no decode)
before any pixels paint. The full decode later replaces the placeholder when `PreloadResponse::Ready` arrives. The thumb
scheduler is paused while `pending_current.is_some()`. See `apps/desktop/src/thumbnails/CLAUDE.md`.

## Loop navigation

Toggled via Navigate → Loop navigation, bare `L` key, or `loop_navigation` MCP tool. Persisted via
`Settings::loop_navigation`. When on, Next at the last image wraps to the first and Previous at the first wraps to the
last. The preloader's active window also wraps so wrap-side neighbours stay warm at the edges.

`App::refresh_preload_window` (formerly `adjust_preload_window_for_loop`) runs on every loop toggle and sort change. It
computes the new active window with `wrap::active_preload_indices`, calls `image_cache.retain_only(&active)` to drop
indices that fall out of the window, then submits preload tasks for newly-in-window indices via the existing
`submit_neighbor_preload` path. Fire-and-forget; the user doesn't wait on these decodes.

## Sort

Choose Name (default), Date, or File type via View → Sort by. All ascending. Persisted via `Settings::sort_by`. The
`SetSortBy` handler re-sorts in place via `DirectoryList::set_sort_by`, which preserves the current image by tracking
its path across the re-sort.

The cache is path-keyed, so it survives a re-sort transparently: in-window entries stay, out-of-window entries get
evicted, missing in-window slots get queued for preload. `refresh_preload_window` does the eviction + queueing.

Before re-sorting, the handler cancels every in-flight preload task. The closures captured `(slot, path)` tuples — the
path is stable, but the slot index now points at a different file in the new ordering, which would mis-target
`pending_current` matching in `poll_preloader`. If a cache-miss target was pending when the user changed the sort, we
re-issue it under its new slot via `prioritize_target`.

## Gotcha/Why: shared state on neighbour-only Ready

`poll_preloader` only used to call `update_shared_state` when the arrived index matched `pending_current`. Background
neighbour decodes (no pending target) inserted into the cache silently, so QA / MCP clients reading
`prvw://state.cache_indices` saw a stale snapshot. We now flip a `neighbor_arrived` flag in the loop and call
`update_shared_state` once after the drain when any non-pending Ready landed.

## Gotchas

- **`zune-jpeg` in debug builds.** Its SIMD is painfully slow without optimizations. `Cargo.toml` sets
  `[profile.dev.package.zune-jpeg] opt-level = 3`.
