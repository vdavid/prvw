# Navigation

Scan the parent directory for images, preload adjacent files in the background, and keep an LRU cache with a flat 512 MB
budget for SDR, twice that for HDR (Phase 5). `SDR_MEMORY_BUDGET` / `HDR_MEMORY_BUDGET` in `preloader.rs` are the
constants, and `ImageCache` resolves both once at construction. The cache switches between them when the RAW pipeline's
`hdr_output` flag flips or the display's EDR headroom crosses the 1.0 boundary; doubling the budget alongside RAW
RGBA16F's doubled per-pixel bytes is what keeps the preload count identical in both modes.

## Decision: the current image is found by name, not by canonicalizing every entry

**Why:** finding the opened image in its folder sits on the launch path, before the first pixel. Resolving every entry
to find the one the user opened costs a filesystem call per file: a cheap `realpath` on APFS, a `CreateFileW` +
`GetFinalPathNameByHandleW` per file on Windows, and a network round trip per file over SMB, which is where the photo
libraries this viewer is built for actually live.

Every entry came from one `read_dir`, so they share a parent and no two of them can share a name. The question splits:
settle the folder once, then compare names. `DirectoryList::from_scan` does exactly that, through
`paths::same_file_name`. Opening a photo in the middle of a 5,000-file folder went from 29 ms to 0.09 ms on local APFS,
the filesystem where the old shape was cheapest.

Numbers, the Windows and SMB reasoning, and the benchmark:
[`docs/notes/directory-index-lookup.md`](../../../../docs/notes/directory-index-lookup.md).

Path comparison itself goes through `crate::paths`, never `==`, so a canonical `\\?\C:\pics\a.jpg` and the plain
`C:\Pics\A.JPG` are one file on Windows and two names on Linux.

## Decision: the preload window is derived from the budget, never stated separately

**Why:** a window wider than the budget retains is worse than a narrow one. Each preload evicts the previous one, and
once the image on screen becomes the LRU entry its own neighbors evict it, so every keypress pays for a decode the
preloader was supposed to have already done. `preload_count()` therefore reads off `SDR_MEMORY_BUDGET`, the same shape
`previews::generation_radius` uses for the same reason, and `MAX_PRELOAD_AHEAD` is a cap rather than the value.

A window of `n` holds `2n + 1` images, so 512 MB covers ±2 of a 24 MP RGBA8 decode (`LARGE_DECODE_BYTES`) with room to
spare. The budget is flat, so that's every machine. Browse mode's pre-warm reads the same `preload_count()`, because it
fills this same cache.

The pairing is checked at **compile time** — a `const _: () = assert!(…)` beside the constants, in the same spirit as
M0.5's parity registries. Lower the budget below what one window costs and the build stops. Raising `MAX_PRELOAD_AHEAD`
can't break it, and that asymmetry is the guarantee: the derivation won't hand out a window the budget doesn't cover.

## Decision: the budget is flat, not RAM-proportional

**Why (David's call):** we want reasonable UX on low-RAM machines too. A viewer that navigates instantly _is_ the
product, and 512 MB is a defensible charge for it even on 8 GB. The alternative shrank exactly the machines least able
to absorb the extra latency.

RAM-proportional scaling is right for `previews`, which spends its budget on a ±50 window of small thumbnails, so more
RAM genuinely buys more of them. It's wrong here in **both** directions:

- **Upward it buys nothing.** The window is capped at ±2, and `App::navigate_by` calls `image_cache.retain_only()` on
  the hot window after every navigation, so the cache is a sliding window rather than an LRU history. A 64 GB machine
  has nothing to spend a bigger budget on — giving it one would need a different retention policy, not a bigger number.
- **Downward it only removes latency budget** from the machine with the least headroom to begin with.

`platform::total_physical_ram_bytes()` stays, because `previews` genuinely uses it.

Full history, the measured tables, and the rejected alternatives:
[`docs/notes/preload-window-and-cache-budget.md`](../../../../docs/notes/preload-window-and-cache-budget.md).

| File             | Purpose                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `mod.rs`         | `navigation::State { dir_list, preloader, image_cache, history, current_image_size, preload_neighbors, pending_current, scan_pending, queued_nav, last_direction, pending_nav_delta, nav_deadline, loop_navigation }`; `PendingScan` + `ScanLanding`; `queue_nav_step` / `queue_nav_jump`; `format_offset` + `NAV_DEBOUNCE` + `LOADING_OVERLAY_DELAY` helpers (byte formatting is `diagnostics::format_bytes`)                                                                        |
| `directory.rs`   | `DirectoryList`: sort, track current position; `provisional(path, sort_by)` (the one-image stand-in used before a folder is scanned) and `from_scan(images, sort_by, current)` (the real list, positioned by name); `from_explicit(files, sort_by, current)` for a multi-file open; `Direction`-aware `preload_range(count, dir, loop_on)`; `go_by(delta, loop_on)`; absolute jumps via `go_to_first()` / `go_to_last()`; `from_sorted(files, sort_by, index)` for live-sync re-scans |
| `folder_diff.rs` | Pure, headless-tested live-sync diff: `diff_folder(old, scanned, sort_by, current)` → adds/removes + the delete-current `CurrentOutcome` (`Unchanged`/`Navigate`/`Empty`). No I/O — the `FolderChanged` handler asks the shared folder scanner and applies the result                                                                                                                                                                                                                 |
| `preloader.rs`   | Serial `std::thread` worker + `ImageCache` with LRU + retain-only eviction; `SDR_MEMORY_BUDGET` / `HDR_MEMORY_BUDGET` and the `preload_count()` derived from them; `read_progress(path)` hands the main thread the live byte counter of an in-flight file read                                                                                                                                                                                                                        |
| `queued_nav.rs`  | Pure, headless-tested `QueuedNav { anchor: NavAnchor, delta }`: the move made before the folder scan landed. `with_step` folds presses together and drops a pair that cancels; `resolve(current, total, loop_on)` wraps or clamps; `direction_hint()` drives preload priority                                                                                                                                                                                                         |
| `wrap.rs`        | Pure-logic loop helpers: `active_preload_indices(current, total, radius, loop_on)`, `step_next` / `step_previous`. Used by `App::refresh_preload_window` on loop toggle / sort change and by `navigate_by` for cache `keep` set                                                                                                                                                                                                                                                       |
| `sort.rs`        | `SortBy { Name, Date, FileType }` (all ascending) + `sort_files()`. Name uses natural alphanumeric (`photo_2 < photo_10`), case-insensitive. Date and FileType fall back to Name as tiebreaker. Date reads each file's mtime exactly once up front, because a comparator that calls `stat` reads every file O(log n) times: thousands of round trips on a share                                                                                                                       |

## State

`App.navigation: navigation::State` owns this feature's runtime. Note the `history` field holds
`VecDeque<NavigationRecord>`. The type is defined in `crate::diagnostics` (it's a measurement record). Navigation pushes
entries; diagnostics formats them.

## Launch and the folder scan

Nothing waits on a directory read. `initialize_viewer` (and the running-app `AppCommand::OpenFile`) installs a
**provisional** `DirectoryList` holding only the opened image, records the folder in `State.scan_pending`, asks
`folder_scan::FolderScanner` for it, and shows the image through `App::display_current_async`. Reading an 8,000-file
folder on an SMB mount takes 17-45 s, and doing it inline meant nothing painted for that long.

- **The display is async for every format**, not just RAW: `pending_current` is set, the window is sized from ImageIO
  dimensions (metadata only), and the preloader's prioritized target ships the pixels. The main thread never decodes a
  launch or open target.
- **The title is the filename alone** while the folder is unknown (a list of one drops the `n/N`).
  `App::install_scanned_folder` fills the position in when `AppCommand::FolderScanned` lands, and also re-seeds the
  preview folder and warms neighbors.
- **The current image is kept by path** across the swap (`DirectoryList::from_scan`), the same invariant `set_sort_by`
  and the live-sync re-scan hold. If it's gone from the listing (deleted while the scan ran), the provisional list stays
  and so does the picture.
- **The pending decode is matched by path**, not by directory slot (`App::pending_slot_for`): the scan landing
  mid-decode moves the image from provisional slot 0 to its real index, so the slot a task was queued under is not a
  stable identity. `PreloadResponse::Ready` and `Preview` carry only the path for this reason.
- **Navigation is queued while `scan_pending` is set**, not dropped: there's nowhere to go in a list of one, so the
  picture stays and the move waits. See "Queued navigation" below.
- **The folder watch starts before the display**, so a file added during a long scan isn't missed.
- `scan_pending` is exposed in `SharedAppState` and the QA `/state`. `PRVW_SCAN_DELAY_MS` delays every scan so tests can
  hold this state (see `launch_shows_the_image_while_the_folder_is_still_being_scanned`).

## The read progress bar

While the "Loading…" overlay is up, a small bar under it fills with the pending image's **real** file read, for every
format that goes through `decoding::load_image`. On a share the read is the wait, so the bar is honest progress rather
than a spinner.

- **The counter is `decoding::ReadProgress`**, an `Arc` of relaxed atomics. `read_file_cancellable` publishes the file
  length from its metadata, then adds each `READ_CHUNK_BYTES` (256 KB) chunk. A metadata call that fails publishes no
  total, and `fraction()` returns `None`, so the bar stays undrawn instead of stuck at zero.
- **The preloader owns the handle.** `Preloader::in_flight` maps each queued or running path to its cancel token and its
  `ReadProgress`; `Preloader::read_progress(path)` hands it to the main thread. The entry outlives the read, so the bar
  holds at full through the decode, which has no honest progress of its own.
- **`App::read_progress_fraction`** joins the two: it returns `None` unless the overlay is visible, so the bar inherits
  the overlay's `LOADING_OVERLAY_DELAY` and a local file flashes neither. `about_to_wait` recomputes it every
  `READ_PROGRESS_POLL` (100 ms) and asks for a redraw only on a change; `schedule_wakeup` adds the poll deadline only
  while a bar is on screen, so an idle viewer never wakes for it.
- **Drawing is `render::progress_bar::build`**, three overlay pills (dark track, thin outline, fill) laid out under the
  "Loading…" pill by `read_bar_top`. The scan status line steps down past the bar when both are up.
- `read_progress` (a `0.0..=1.0` fraction, or null) is exposed in `SharedAppState` and the QA `/state`.
  `PRVW_READ_DELAY_MS` pauses the read before every chunk so tests can watch the bar climb on a local file (see
  `the_read_progress_bar_climbs_while_a_slow_file_reads`).

The multi-file launch (`explicit_files`) skips all of this: the given files are already the navigation set.

## Queued navigation

A move asked for while the folder is still being scanned is recorded as a `queued_nav::QueuedNav` — an anchor (`Current`
/ `First` / `Last`) plus a signed step count — and applied the moment the real folder arrives. The picture on screen
doesn't move meanwhile.

- **Everything queues.** `App::queue_nav_while_scanning` covers arrow keys and the wheel (`App::queue_nav_step`),
  Next/Previous and the immediate QA/MCP path (`navigate_by`), and a slideshow advance; `App::queue_jump_while_scanning`
  covers Home / End. All four return `true` to tell the caller to stand down.
- **Steps fold, jumps replace.** Left then right nets to zero and clears the queued move entirely
  (`queued_nav::with_step`); Home / End reset the anchor and the step count, then further arrows walk from there.
- **Resolution wraps or clamps.** `QueuedNav::resolve` takes the anchor index, adds the delta, and wraps when loop
  navigation is on or clamps to the folder edges when it's off — the same thing the key does on a scanned folder.
- **It lands through the normal path.** `App::apply_queued_nav` runs from `install_scanned_folder` and goes through
  `after_position_change`, so preloads, previews, the title, and the navigation history behave as they do for a live key
  press. It runs before the stay-put neighbor warm-up, which would otherwise warm a folder position we're leaving.
- **A slideshow advance queues like a key press** and keeps its dwell timer, so every tick that passes during the scan
  adds a step and the show resumes where it would have been.
- **Opening another image drops it.** The `OpenFile` handler clears `queued_nav` with the old folder, and so does an
  `install_scanned_folder` that can't find the current image in the listing.
- `queued_nav` is exposed in `SharedAppState` and the QA `/state` as `{anchor, delta}` or `null`.

## Scan status text

While a queued move waits on a folder read, image mode draws a quiet glyphon line under the "Loading…" slot: "Scanning
folder… 3,412 images so far" (`App::image_scan_status`, drawn in `build_text_overlay`). It's deliberately smaller,
unbolded, and dimmer than the overlay above it — information, not a headline. Nothing shows when no move is queued: the
main screen doesn't advertise a scan nobody is waiting on.

The count comes from `folder_scan::FolderScanner::progress`, and the text holds off until the scan has been reading for
`folder_scan::STATUS_DELAY` (150 ms), so a folder that lists instantly never flashes it. `App::about_to_wait` recomputes
the line and asks for a redraw only when it changes; `schedule_wakeup` holds a `WaitUntil` at `folder_scan::STATUS_POLL`
(250 ms) while `App::scan_status_needs_polling` is true, which is what keeps the number ticking on an otherwise idle
loop. The browse grid and tree show the same count through `App::refresh_browse_scan_status` — see `browser/CLAUDE.md`.

## Navigation render path

On cache hit, `navigate_by` renders from cache synchronously and submits neighbor preloads via
`Preloader::request_neighbor_preload`. On cache miss it sets `State.pending_current = Some(index)`, shows a "Loading…"
title, and calls `Preloader::prioritize_target(index, path, total)` — which cancels every other in-flight task so the
priority-0 target gets the worker's full attention. `poll_preloader` runs the render when
`PreloadResponse::Ready { path }` matches the file `pending_current` points at, then queues the now-displayed image's
neighbors (deferred until after the target arrives — see "Preloader prioritization" below). The main thread never
decodes navigation targets directly. Only settings re-decode and `Refresh` still call the sync `display_image` path.

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
any user event, which is where we drain the channel. Same pattern the preview completion path uses.

This was masked when neighbor preload was always-on (constant decode activity kept the loop awake) and surfaced only
after the deferred-neighbor change reduced background work. If you add another async-result path, include the same
`send_event` wakeup or the result will be silently delayed.

## Preview placeholder

On a cache-miss navigation the title bar shows "Loading…", and a centered "Loading..." pill appears mid-screen once the
target has taken longer than `LOADING_OVERLAY_DELAY` (150 ms). A local file decodes well inside that, so it never
flashes. `App::about_to_wait` recomputes `App.loading_overlay_visible` and asks for a redraw only when it flips;
`schedule_wakeup` holds a `WaitUntil` at the deadline so the overlay still appears on an otherwise idle loop.
`apply_preview_auto_fit` resizes the window to the source dimensions (read from the file's header by
`previews::metadata`, no decode) before any pixels paint, on **every** platform. On macOS, if the `previews` module also
has a cached QuickLook preview for the target index, that preview is uploaded to the image texture as a blurry
placeholder and carries the resize itself; elsewhere the cache is always empty, so the fit is all of it and the window
shows black until the decode lands. The full decode later replaces the placeholder when `PreloadResponse::Ready`
arrives. The preview scheduler is paused while `pending_current.is_some()`. See `apps/desktop/src/previews/CLAUDE.md`.

## RAW quick preview

RAW develops are slow (~450 ms for 20 MP), so a cache-miss to a RAW would otherwise sit on the "Loading…" pill until the
develop finishes. To fill that gap, the priority-target task (`queue_task` with `wants_preview = true`, set only by
`prioritize_target`) extracts the camera's **embedded JPEG preview** via `decoding::decode_raw_preview` _before_ running
the develop, and ships it as `PreloadResponse::Preview`. `poll_preloader` shows it via
`App::display_preview_placeholder` — but only while `pending_current` still matches (a newer nav drops it). It's
deliberately downscaled (~1024 px long edge) so it reads as a soft placeholder, not a finished image: the camera's JPEG
look differs from our develop, and the softness makes the sharp `Ready` swap read as snapping into focus rather than a
confusing change. Not cached — purely transient. RAW-only (JPEG/generic decode fast enough not to need it). Neighbors
never request a preview (they're never displayed yet). The `Preview` decode is cross-platform, but its _display_
(`App::display_raw_preview_placeholder`) is `#[cfg(target_os = "macos")]`, so non-macOS builds drop the `Preview` arm.
Off macOS a RAW cache-miss therefore fits the window from the header and then waits on the develop with a black frame.
Not `display_preview_placeholder`, which is the QuickLook path and compiles everywhere; it just finds an empty cache and
returns `false`.

**Launch and open take the same path, for every format.** `App::display_current_async` (called from `initialize_viewer`
and the `OpenFile` handler) mirrors the cache-miss nav flow: set `pending_current`, size the window from header dims via
`apply_preview_auto_fit`, call `prioritize_target`. A RAW gets its embedded preview instantly instead of blocking on the
~450 ms develop, and everything else gets the same non-blocking treatment, which is what keeps a slow network read off
the main thread. Ordering matters inside it: the preview folder is seeded BEFORE the auto-fit (which reads source
dimensions through `previews`), and `initialize_viewer` pauses the preview scheduler AFTER the display, so the path's
`pending_current` gates the pause.

## Loop navigation

Toggled via Navigate → Loop navigation, bare `L` key, or `loop_navigation` MCP tool. Persisted via
`Settings::loop_navigation`. When on, Next at the last image wraps to the first and Previous at the first wraps to the
last. The preloader's active window also wraps so wrap-side neighbours stay warm at the edges.

`App::refresh_preload_window` runs on every loop toggle and sort change. It computes the new active window with
`wrap::active_preload_indices`, calls `image_cache.retain_only(&active)` to drop indices that fall out of the window,
then submits preload tasks for newly-in-window indices via the existing `submit_neighbor_preload` path. Fire-and-forget;
the user doesn't wait on these decodes.

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

## Live folder sync (image mode)

The active folder — the current image's parent — is watched live, so adds/modifies/deletes reflect without a manual
refresh. The watcher infra lives in `crate::folder_watch` (a `notify` FSEvents watcher + a pure debounce/coalescer); the
re-read it triggers rides the shared `crate::folder_scan` worker. This is the image-mode consumer.

**Flow.** `App::retarget_active_folder_watch` watches the current image's folder (unwatching the previous one) on every
active-folder change: launch, `OpenFile`, browse reveal (`reveal_selected_image`), and a re-scan that empties the
folder. A coalesced `AppCommand::FolderChanged { folder, modified }` arrives on the main thread;
`App::handle_folder_changed` evicts the `modified` paths from `image_cache` (`ImageCache::remove`) and the preview cache
(`previews::forget_path`), then asks the shared `folder_scan::FolderScanner` to re-read the folder (never `read_dir`
inline — a slow SMB folder must not block the loop) and notes it in `App.pending_rescan`. The result returns as
`AppCommand::FolderScanned`, which `App::handle_folder_scanned` routes to `App::apply_folder_rescan`.

**Applying the diff** (`apply_folder_rescan` → pure `folder_diff::diff_folder`):

- **`Unchanged`** — adds/removes shifted around the current image; rebuild the list via `DirectoryList::from_sorted`
  keeping the current image **by path** (the same invariant as `set_sort_by`). If a modified path is the displayed
  image, re-decode it via `refresh_current_after_modify` (cache was evicted → fresh bytes through the normal
  `prioritize_target` path).
- **`Navigate { index }`** — the current image was deleted; land on the next surviving image (or the new last if it was
  last) via `display_after_delete` (instant from cache, else the async placeholder path).
- **`Empty`** — the folder has no images left; `enter_no_images_state` drops `dir_list`, clears the bound texture (black
  canvas), and flags `no_images_empty_state` so `render_frame` draws the centered "(No images)" glyphon overlay. The
  watch stays on the folder, so a newly-added image clears the empty state and opens (`Navigate { 0 }`).

`SharedAppState.no_images` (and the `/state` `no_images` field) expose the empty state to QA/tests; the image-mode
`live_sync_*` tests (in `tests/e2e_shared.rs`, so they run wherever the app does) drive the real watcher with a temp
folder.

**Requested is not armed.** `retarget_active_folder_watch` only queues the watch; the worker applies it, and FSEvents
reports nothing that happened before its stream started. `/state`'s `watched_folders` lists what's actually applied, and
every `live_sync_*` test polls it (`TestApp::wait_for_watch`) before mutating its folder. See `qa/CLAUDE.md`.

**The active folder is the grid's in browse mode, on every platform that has a browser.** `App::active_folder` reads
`browser::selected_folder()` there and the current image's parent otherwise. The browse branch is gated on
`any(macos, windows)`: with it macOS-only, a Windows dir-arg launch had no image at all, so nothing was watched and
browse-mode live sync did nothing. `watched_folders` reports the folder as the tree spelled it, which needn't be the
canonical spelling a test would canonicalize to — `wait_for_watch` compares under the platform's path rules for that
reason.

## Gotcha/Why: shared state on neighbour-only Ready

`poll_preloader` only used to call `update_shared_state` when the arrived index matched `pending_current`. Background
neighbour decodes (no pending target) inserted into the cache silently, so QA / MCP clients reading
`prvw://state.cache_indices` saw a stale snapshot. We now flip a `neighbor_arrived` flag in the loop and call
`update_shared_state` once after the drain when any non-pending Ready landed.

## Salvaged decodes (cancelled-but-completed)

A cancelled JPEG/generic decode runs to completion on its detached thread (see `decoding::run_decode_cancellable` — we
can't safely kill it mid-flight). Rather than discard that finished image, the preloader builds a `SalvageSink` per task
that ships it back as `PreloadResponse::Salvaged`. `poll_preloader` then keeps it **only if** the path is still in the
hot window (`current_window_keep_paths`, the same `active_preload_indices` set used for eviction) **and** not already
cached; otherwise it drops it, honoring the respect-resources policy (no out-of-window image squatting in RAM). Salvaged
images are deliberately not used to satisfy `pending_current` — the prioritized fresh decode owns the user-visible
target; salvage only warms the cache. A kept salvage flips `neighbor_arrived` so shared state reflects the new
`cache_indices`.

## Gotchas

- **`zune-jpeg` in debug builds.** Its SIMD is painfully slow without optimizations. `Cargo.toml` sets
  `[profile.dev.package.zune-jpeg] opt-level = 3`.
