# Navigation

Scan the parent directory for images, preload adjacent files in the background, and
keep an LRU cache budgeted at 512 MB (SDR) or 1 GB (HDR, Phase 5). The cache
auto-scales when the RAW pipeline's `hdr_output` flag flips or the display's EDR
headroom crosses the 1.0 boundary, so preload count stays constant as we double
per-pixel bytes for RAW RGBA16F.

| File           | Purpose                                                                                    |
| -------------- | ------------------------------------------------------------------------------------------ |
| `mod.rs`       | `navigation::State { dir_list, preloader, image_cache, history, current_image_size, preload_neighbors, pending_current, last_direction, pending_nav_delta, nav_deadline }`; `format_offset` + `format_bytes` + `NAV_DEBOUNCE` helpers |
| `directory.rs` | `DirectoryList` — scan parent dir for supported extensions, sort, track current position; `Direction`-aware `preload_range`; `go_by(delta)` |
| `preloader.rs` | Serial `std::thread` worker + `ImageCache` with LRU + retain-only eviction (512 MB / 1 GB budget)                                           |

## State

`App.navigation: navigation::State` owns this feature's runtime. Note the `history`
field holds `VecDeque<NavigationRecord>` — the type is defined in `crate::diagnostics`
(it's a measurement record). Navigation pushes entries; diagnostics formats them.

## Navigation render path

On cache hit, `navigate_by` renders from cache synchronously and submits
neighbor preloads. On cache miss it sets `State.pending_current = Some(index)`,
shows a "Loading…" title, and submits the target as the priority-zero preload
task (first entry in `request_preload`'s `tasks` list → FIFO slot).
`poll_preloader` runs the render when `PreloadResponse::Ready { index }`
matches `pending_current`, then clears it. The main thread never decodes
navigation targets directly — only settings re-decode and `Refresh` still call
the sync `display_image` path.

## Debounced navigation

User input (arrow keys, mouse wheel, Next/Previous menu items) goes through
`AppCommand::NavigateDebounced`, which accumulates a signed delta in
`State.pending_nav_delta` and sets `State.nav_deadline` to now +
`NAV_DEBOUNCE` (30 ms). `App::about_to_wait` fires the flush when the deadline
elapses; winit gets `ControlFlow::WaitUntil(deadline)` so the wake is precise.
A sustained wheel spin collapses into a single `navigate_by(±20)` jump with
one decode, not twenty. QA / MCP / HTTP use the immediate `AppCommand::Navigate`
path, which flushes pending first so automated tests see deterministic state.

## Key patterns

- **Dedicated `std::thread` worker, not a rayon pool.** Tasks are queued
  through an `mpsc::channel` to a single OS thread that pops and runs them
  serially. Responses come back via another `mpsc::channel`. An in-flight
  `HashMap<index, Arc<AtomicBool>>` lets us cancel only the tokens for
  indices that dropped out of the priority list; tasks still wanted keep
  their existing token.

  **Why not rayon?** rawler's internal `par_iter` inherits the caller's
  rayon pool. On a 1-thread custom pool, rawler's parallel stages
  (demosaic, chroma_nr, sharpen) collapse to 1 thread and balloon ~10×.
  A plain OS thread isn't a rayon worker, so `par_iter` inside it falls
  back to the global pool (every logical core), matching the main-thread
  sync decode path. See the comment block above `Preloader` in
  `preloader.rs` for the measurement table.
- **Direction-aware priority.** `DirectoryList::preload_range` takes a
  `Direction` (forward / backward / unknown) and returns indices ordered by
  likelihood of being viewed next. Forward nav returns `[N+1, N+2, N-1, N-2]`;
  `navigate_by` in `app.rs` prepends the current index when it's uncached,
  submits the full list to `Preloader::request_preload`, and the channel is
  naturally FIFO so submission order = execution order.
- **Cancellation.** Preload tasks hold an `Arc<AtomicBool>`; navigation away
  flips the tokens for any indices no longer in the priority list. Tasks
  still wanted keep their existing token and don't restart mid-decode.
- **Supported extensions are decided by `decoding`** — `DirectoryList` filters via
  `decoding::is_supported_extension`. New format support = one change, two effects
  (decode + list).
- **Preload can be disabled for benchmarking.** `State.preload_neighbors` (driven
  by Settings → General → "Preload next/prev images", default on) gates both
  `preloader.request_preload` call sites in `app.rs`. When off, only the
  currently-displayed image consumes decode work — intended for single-image
  cold-start perf measurements where concurrent preloads would skew the
  per-stage timings logged by `decoding::raw::decode`.

## Gotchas

- **`zune-jpeg` in debug builds.** Its SIMD is painfully slow without optimizations.
  `Cargo.toml` sets `[profile.dev.package.zune-jpeg] opt-level = 3`.
