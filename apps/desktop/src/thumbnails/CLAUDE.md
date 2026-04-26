# Thumbnails (macOS-only)

Background-generate thumbnails for every file in the current folder so navigating to an image outside the full-decode
preload window shows a blurry placeholder instantly instead of a blank screen. Relies on macOS's system-wide QuickLook
thumbnail cache (`quicklookd`), shared with Finder, Preview, and every other Mac app — no disk storage of our own.

| File              | Purpose                                                                               |
| ----------------- | ------------------------------------------------------------------------------------- |
| `mod.rs`          | `State { scheduler, cache, dim_prefetcher, paths, requests }` + public API + eviction |
| `scheduler.rs`    | Pure state machine: priority-ordered queue, windowing, parallelism cap, pause/resume  |
| `metadata.rs`     | Three-tier dim+orientation reader (image crate / image+nom-exif / ImageIO)            |
| `dim_prefetch.rs` | 16-thread parallel pool that pre-warms `(width, height)` for window indices           |
| `quicklook.rs`    | QL submission worker thread + `Retained<...>` lifecycle + `CGImage → RGBA8` blit      |

## Flow

1. `App::resumed` calls `State::set_folder(paths, current)` after the directory scan.
2. The scheduler enqueues every folder index, ordered centered-outward but with indices inside the full-decode preload
   window (`|i − current| ≤ 2`) pushed last — the full-decode preloader will cover those anyway, so they're the
   lowest-value thumbs to fetch.
3. `App::pump_thumbnail_requests` drains the scheduler up to `max_parallel` (`available_parallelism() / 2`, min 1) and
   submits each to `QLThumbnailGenerator`.
4. `quicklookd` generates (or cache-hits) a 512 × scale thumb and calls our completion block on its internal queue.
5. The block converts the `CGImage` to RGBA8 and fires `AppCommand::ThumbnailReady { index, rgba, width, height }` via
   `EventLoopProxy`, which `winit` delivers as a `user_event` on the main thread.
6. `App::execute_command` stores the thumb in the cache and — if this thumb is for `pending_current` — uploads it into
   the image texture as a placeholder. The full decode later replaces it.

## Key patterns

- **No dedicated thread.** `QLThumbnailGenerator` is async inside the system (`quicklookd` runs out-of-process). Main
  thread submits; completion blocks forward results as winit user events. Wrapping this in a worker thread would just
  proxy through an already-async API.
- **System cache, not ours.** `quicklookd` maintains an on-disk cache keyed by file URL + mtime. Modified files
  auto-invalidate, which means we never check staleness ourselves. Disk cost: zero; cache hygiene: OS-managed.
- **Raw FFI for CF / CG / ImageIO.** `objc2-*` 0.3 ships bindings for a lot but not `CGBitmapContextCreate` (only the
  new adaptive variant). The few calls we need are declared in `extern "C"` blocks locally, matching the pattern in
  `color::display_profile`.
- **ImageIO dims before thumb pixels.** `apply_thumbnail_auto_fit` runs on cache-miss navigation to resize the window to
  the final image size _before_ any pixels paint. The thumb (and later the full decode) then fill the already-correct
  window. No second resize when the full decode lands — the numbers match.
- **Thumb routes through the same display pipeline as full images.** `display_thumbnail_placeholder` calls
  `App::prepare_display(source_w, source_h, false)` → `renderer.set_image(thumb)` → `App::finalize_display()`, exactly
  like `display_from_cache` does for full images. The shared helpers handle window auto-fit, EDR surface state, and
  `apply_initial_zoom`. Without this, thumbs landed at whatever zoom the previous image left behind (often 1:1, looking
  like a crop). Linear sampling on upscale provides a soft, blurred appearance that signals "not final." A dedicated
  blur shader is not currently implemented — the linear-sampler softness is sufficient.

## Pause semantics

While a primary decode is pending (`navigation.pending_current.is_some()`), the scheduler is paused so it doesn't
compete for I/O or shared system CPU. Resumed on decode completion (success or failure). Already-in-flight requests keep
running — cancellation has I/O cost and `quicklookd` is usually near-done.

## Windowed scheduling + cache eviction (10k folders)

Two constants govern memory and CPU footprint for large folders:

- `scheduler::WINDOW_RADIUS` (50): the scheduler only enqueues thumbnail jobs for indices in `current ± WINDOW_RADIUS`.
  Reseeded on every `set_current`. For a 10k-image folder we'd otherwise queue 10 000 jobs at startup — ~24 min at
  quicklookd's ~7/sec serving rate — for thumbs the user mostly never looks at. Now ~100 around the user populates in
  ~14 seconds.

- `RETENTION_RADIUS` (200): thumbnails outside this distance from `current` are evicted from the in-memory cache on
  `set_current`. Caps RAM at ~1.2 GB peak (~400 thumbs × 3 MB RGBA8) for huge folders no matter how much the user
  navigates. Eviction also drops the index from the scheduler's `cached` set via `uncache(idx)` so re-entering that area
  re-enqueues the thumb.

Larger than `WINDOW_RADIUS` so a small nav doesn't immediately re-evict thumbs we just generated; the gap is "navigation
slack" before paying to regenerate.

Trade-off: a far jump (#5000 → #100) blanks the new neighborhood of thumbs (we never generated them) and evicts the old
(now > RETENTION*RADIUS away). New neighborhood populates over ~14 seconds. Going \_back* is fast — quicklookd's
persistent disk cache survives our in-RAM eviction, so subsequent visits hit cached thumbs at ~150 ms each instead of
~840 ms first-gen.

## QL submission threading (option A)

`RequestTable` runs a dedicated `prvw-thumbgen` worker thread that owns the
`entries: HashMap<RequestId, Retained<QLThumbnailGenerationRequest>>` and the `QLThumbnailGenerator` singleton (created
on that thread via `sharedGenerator`). All ops on the main thread are mpsc sends:

| Main → Worker             | Worker action                                                                |
| ------------------------- | ---------------------------------------------------------------------------- |
| `WorkerMsg::Submit { … }` | Build `NSURL`, request, generation, store in `entries`, fire `generateBest…` |
| `WorkerMsg::Forget(id)`   | `entries.remove(id)` (sent by completion block after it delivers)            |
| `WorkerMsg::CancelAll`    | `entries.drain().for_each(cancelRequest)` (called on folder change)          |

Why a worker: `NSURL::fileURLWithPath` + `generateBest…` cost ~150 ms each on a slow SMB share. Doing 7 of those per
pump cycle on the main thread blocked rendering and froze the UI for seconds.

Why the worker owns `entries`: `Retained<QLThumbnailGenerationRequest>` isn't `Send`-friendly. Keeping the map on the
worker side means we never need to share retained ObjC pointers across threads.

## Dimension prefetcher (16-thread pool)

`dim_prefetch::DimPrefetcher` runs 16 worker threads (named `prvw-dim-N`, 2 MB stack each ≈ 32 MB total) that read pixel
dimensions for every index in `current ± WINDOW_RADIUS` in parallel. Results land in an
`Arc<Mutex<HashMap<usize, Dimensions>>>` that the main thread reads when a placeholder needs to display.

**Why:** without this, each first-time placeholder display paid the cost of one synchronous ImageIO file-header read on
the main thread — 200 ms – 1.3 s per file on slow SMB shares. With the prefetcher, every nav finds dims pre-cached and
the placeholder shows in <5 ms.

**Why 16 threads:** SMB allows ~64 outstanding requests per session (~20 concurrent file ops at ~3 ops/file). 16 keeps
us safely under that ceiling and well above the previously-considered 8. Local SSD can go higher; iCloud Drive less. 16
is the all-rounder.

**Generation guard:** `DimPrefetcher::reset()` (called on folder change) bumps an `AtomicU64`. In-flight workers that
finish on a stale generation drop their result. Keeps the cache truthful across folder changes without per-job
cancellation.

**Lazy fallback:** if the user navigates faster than the prefetcher, `State::source_dimensions` falls through to a
synchronous `metadata::read_dimensions_fast` call on the main thread — same three-tier dispatcher the workers use, just
blocking. Result is cached back into the prefetcher's map for next time.

## Three-tier dim+orientation reader

`metadata::read_dimensions_fast(path)` dispatches by extension. Goal: _one_ file open per file regardless of which tier
handles it.

| Tier | Formats                        | Reader                                                                  | Why                                                                   |
| ---- | ------------------------------ | ----------------------------------------------------------------------- | --------------------------------------------------------------------- |
| 1    | PNG, GIF, BMP                  | `image::image_dimensions`                                               | No EXIF needed; tiny header; pure-Rust                                |
| 2    | JPEG                           | open once → 64 KB buffer → `image` for dim + `nom-exif` for orientation | Both parsers run in-memory on the same buffer; single SMB RTT         |
| 3    | RAW, HEIC, WebP, TIFF, unknown | `read_dimensions` (ImageIO)                                             | Format coverage is the priority; ImageIO handles them all in one pass |

For tier 2, 64 KB is well above any JPEG's SOF + APP1/EXIF segments (typically both within the first 4 KB), so a single
read covers both parsers. `nom_exif::parse_jpeg_exif` is deprecated in favor of the `MediaParser` API but still works
for one-shot reads — kept with `#[allow(deprecated)]` until upstream removes it.

## Scheduler priority order

`scheduler::rebuild_queue` produces this order around `current`:

1. **Phase 1** — immediate neighbors (`dist 1..=PRELOAD_HALF`), centered outward. Most likely next nav target. Must have
   a placeholder ready _before_ the user presses arrow.
2. **Phase 2** — outside preload window (`dist > PRELOAD_HALF`), centered outward, capped by `WINDOW_RADIUS`.
   Exploration thumbs.
3. **Phase 3** — `current` itself, last. The primary decode is what we display; a thumb for it is redundant.

The original order was Phase 2 → Phase 1 → current; that was wrong because a 7-image-per-second QL serving rate meant
immediate-neighbor thumbs arrived ~5 s after launch — leaving the user's first arrow-key press without a placeholder.
Swapping made the warm-up window for "the most likely nav target" go from ~5 s to <1 s.

## Thumbnail size

`CGSize { 512, 512 }` at `NSScreen.backingScaleFactor` (2.0 Retina → 1024 effective pixels). This matches QuickLook's
gallery cache bucket, so folders the user has browsed in Finder's gallery view hit the cache instantly. Above 1024
effective, `quicklookd` renders from source every time — falls off the cache entirely.

## MCP

Exposed via the `thumbnails_status` MCP tool. Returns folder length, current index, in-flight indices, queue length,
cached indices, failed indices, paused flag, and parallelism cap.

## Gotchas

- **`QLThumbnailRepresentation::CGImage()` returns a `Retained<CGImage>` wrapper.** Pass its raw pointer via
  `Retained::as_ptr` to the FFI `CGContextDrawImage`; the `Retained` drops at end-of-scope and releases naturally.
- **Completion blocks run on quicklookd's queue.** `EventLoopProxy` is `Send`, so forwarding is fine, but don't touch
  `App` fields inside the block — the block exists only to post a user event. All mutation happens on the main thread.
- **Cancellation semantics.** We don't cancel individual in-flight requests mid-flight. On folder change,
  `RequestTable::cancel_all` fires `cancelRequest` on every live request. Individual cancellation isn't wired because
  the scheduler's queue-based model covers the common case.

## Future work

- **File watcher integration.** `State::set_folder` is the single entry point for the path list. When `fsevents`
  watching lands, it just calls `set_folder` again with the updated list — scheduler cancels orphaned requests and
  enqueues new ones.
- **Dedicated blur shader.** Current: linear-sampler softening on upscale. A separable Gaussian in a fragment shader
  would look nicer behind the "Loading..." overlay. Not required for correctness.
