# Previews

Two halves that share a folder list, with different reach:

- **Dimensions, everywhere.** Read `(width, height)` from a file's header without decoding it, so the window can
  auto-fit to the final image size before the first pixel paints. Runs on macOS, Windows, and Linux.
- **Preview pixels, macOS only.** Background-generate previews for every file in the current folder so navigating to an
  image outside the full-decode preload window shows a blurry placeholder instantly instead of a blank screen. Relies on
  macOS's system-wide QuickLook preview cache (`quicklookd`), shared with Finder, Preview, and every other Mac app — no
  disk storage of our own. Windows gets its own generator in M3 (`docs/specs/cross-platform-plan.md`).

| File              | Purpose                                                                                                          | Platforms |
| ----------------- | ---------------------------------------------------------------------------------------------------------------- | --------- |
| `mod.rs`          | `State { scheduler, cache, dim_prefetcher, paths, current, requests }` + API + RAM-scaled byte budget + eviction | all       |
| `metadata.rs`     | Four-tier dim+orientation reader (rawler / image / image+nom-exif)                                               | all       |
| `dim_prefetch.rs` | 16-thread parallel pool that pre-warms `(width, height)` for window indices                                      | all       |
| `scheduler.rs`    | Pure state machine: priority-ordered queue, windowing, parallelism cap, pause/resume                             | macOS     |
| `quicklook.rs`    | QL submission worker thread + `Retained<...>` lifecycle + `CGImage → RGBA8` blit                                 | macOS     |

Only `quicklook.rs` is `#[cfg]`ed out off macOS. The scheduler compiles everywhere and its queue is seeded everywhere,
but nothing drains it without a preview generator, so `mod previews;` in `main.rs` carries a module-level dead-code
allow off macOS — the same shape `mod parity;` uses, and for the same reason. macOS stays the build that catches a
member nothing reads any more.

## Flow

1. `App::resumed` calls `State::set_folder(paths, current)` after the directory scan.
2. The scheduler enqueues every folder index, ordered centered-outward but with indices inside the full-decode preload
   window (`|i − current| ≤ 2`) pushed last — the full-decode preloader will cover those anyway, so they're the
   lowest-value previews to fetch.
3. `App::pump_preview_requests` drains the scheduler up to `max_parallel` (`available_parallelism() / 2`, min 1) and
   submits each to `QLThumbnailGenerator`.
4. `quicklookd` generates (or cache-hits) a 512 × scale preview and calls our completion block on its internal queue.
5. The block converts the `CGImage` to RGBA8 and fires `AppCommand::PreviewReady { index, rgba, width, height }` via
   `EventLoopProxy`, which `winit` delivers as a `user_event` on the main thread.
6. `App::execute_command` stores the preview in the cache and — if this preview is for `pending_current` — uploads it
   into the image texture as a placeholder. The full decode later replaces it.

## Key patterns

- **No dedicated thread.** `QLThumbnailGenerator` is async inside the system (`quicklookd` runs out-of-process). Main
  thread submits; completion blocks forward results as winit user events. Wrapping this in a worker thread would just
  proxy through an already-async API.
- **System cache, not ours.** `quicklookd` maintains an on-disk cache keyed by file URL + mtime. Modified files
  auto-invalidate, which means we never check staleness ourselves. Disk cost: zero; cache hygiene: OS-managed.
- **Raw FFI for CF / CG / ImageIO.** `objc2-*` 0.3 ships bindings for a lot but not `CGBitmapContextCreate` (only the
  new adaptive variant). The few calls we need are declared in `extern "C"` blocks locally, matching the pattern in
  `color::display_profile`.
- **Dimensions before pixels.** `apply_preview_auto_fit` runs on cache-miss navigation (and on a RAW launch) to resize
  the window to the final image size _before_ any pixels paint. The preview, and later the full decode, then fill the
  already-correct window. No second resize when the full decode lands — the numbers match, by construction: see the
  decision below.
- **Preview routes through the same display pipeline as full images.** `display_preview_placeholder` calls
  `App::prepare_display(source_w, source_h, false)` → `renderer.set_image(preview)` → `App::finalize_display()`, exactly
  like `display_from_cache` does for full images. The shared helpers handle window auto-fit, EDR surface state, and
  `apply_initial_zoom`. Without this, previews landed at whatever zoom the previous image left behind (often 1:1,
  looking like a crop). Linear sampling on upscale provides a soft, blurred appearance that signals "not final." A
  dedicated blur shader is not currently implemented — the linear-sampler softness is sufficient.

## Pause semantics

While a primary decode is pending (`navigation.pending_current.is_some()`), the scheduler is paused so it doesn't
compete for I/O or shared system CPU. Resumed on decode completion (success or failure). Already-in-flight requests keep
running — cancellation has I/O cost and `quicklookd` is usually near-done.

## RAM-scaled byte budget + windowed scheduling (10k folders)

Memory and CPU footprint for large folders are governed by a single **RAM-proportional byte budget**, with the
generation window derived from it so the two never fight.

- **`preview_budget_bytes()`** (`mod.rs`): `clamp(physical_RAM / 128, 64 MB, 1 GB)`. 64 GB → 512 MB, 16 GB → 128 MB, 8
  GB → 64 MB (floor). A byte budget (not a fixed preview count) so it self-adjusts to preview size and display DPI.
  Physical RAM comes from `platform::total_physical_ram_bytes()`, queried once (`sysctl hw.memsize` on macOS,
  `GlobalMemoryStatusEx` on Windows, `/proc/meminfo` on Linux). `navigation::preloader` deliberately does **not** scale:
  its window is capped at ±2 and it drops everything outside that window on every navigation, so a bigger budget has
  nothing to buy there. It does derive its window from its budget, the way `generation_radius` does here.

- **Eviction (`evict_to_budget`)**: on `set_current` _and_ on each preview's arrival (`mark_ready`), evict
  farthest-from-`current` first until total bytes ≤ budget. Distance-based, so we always keep the previews nearest where
  the user is — never a stale trail from where they _were_. Each eviction also drops the index from the scheduler's
  `cached` set (`uncache`) and the dim cache, so re-entering an area re-enqueues it.

- **`scheduler::WINDOW_RADIUS` (50)** is now the _cap_ on the generation radius, not a fixed value. The effective radius
  is `generation_radius() = min(50, budget / (2 × ~4 MB))`, injected via `Scheduler::with_window_radius`. This keeps
  generation ≤ retention: we never ask quicklookd to produce previews the byte budget would evict on arrival (which
  would churn it nonstop on small-RAM machines). 64 GB → radius 50; 16 GB → ~16; 8 GB → ~8. The dim-prefetch window uses
  the same effective radius via `scheduler.window_radius()`.

Trade-off: a far jump (#5000 → #100) blanks the new neighborhood (never generated) and evicts the old (now farthest).
The new neighborhood repopulates over a few seconds. Going _back_ is fast — quicklookd's persistent disk cache survives
our in-RAM eviction, so revisits hit cached previews at ~150 ms each instead of ~840 ms first-gen.

The pure math (`budget_for_ram`, `generation_radius_for_budget`) and the distance-based eviction policy are unit-tested
in `mod.rs`. `State::memory_bytes()` exposes the resident total for the diagnostics overlay (`process_memory` line
breaks out image cache vs. previews so the gap to RSS — GPU texture, decode buffers, allocator retention — is visible).

## QL submission threading (option A)

**Shared with the browse grid.** `RequestTable::new(wake, thread_name)` is parameterized on the wake `AppCommand`
constructor and worker-thread name, so the browse grid (`browser::grid`) owns a **second** `RequestTable` — a second
request path into the same shared `quicklookd` cache (`QLThumbnailGenerator` singleton), not a second engine. Both
deliver RGBA8 (`cg_image_to_rgba8`); they differ only in the wake command (`PreviewsAvailable` vs
`BrowseThumbnailsAvailable`) and how the main thread consumes the bytes: previews blit to a wgpu texture, the grid wraps
them in an `NSImage` via `quicklook::nsimage_from_rgba8` (the documented seam at the top of `quicklook.rs`). Previews
behavior is unchanged.

`RequestTable` runs a dedicated worker thread (`prvw-previewgen` for previews, `prvw-gridgen` for the grid) that owns
the `entries: HashMap<RequestId, Retained<QLThumbnailGenerationRequest>>` and the `QLThumbnailGenerator` singleton
(created on that thread via `sharedGenerator`). All ops on the main thread are mpsc sends:

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
dimensions for every index in the generation window (`current ± scheduler.window_radius()`) in parallel. Results land in
an `Arc<Mutex<HashMap<usize, Dimensions>>>` that the main thread reads when a placeholder needs to display.

**Why:** without this, each first-time placeholder display paid the cost of one synchronous file-header read on the main
thread — 200 ms – 1.3 s per file on slow SMB shares. With the prefetcher, every nav finds dims pre-cached and the
placeholder shows in <5 ms.

**Why 16 threads:** SMB allows ~64 outstanding requests per session (~20 concurrent file ops at ~3 ops/file). 16 keeps
us safely under that ceiling and well above the previously-considered 8. Local SSD can go higher; iCloud Drive less. 16
is the all-rounder.

**Generation guard:** `DimPrefetcher::reset()` (called on folder change) bumps an `AtomicU64`. In-flight workers that
finish on a stale generation drop their result. Keeps the cache truthful across folder changes without per-job
cancellation.

**Lazy fallback:** if the user navigates faster than the prefetcher, `State::source_dimensions` falls through to a
synchronous `metadata::read_dimensions_fast` call on the main thread — same three-tier dispatcher the workers use, just
blocking. Result is cached back into the prefetcher's map for next time.

## Four-tier dim+orientation reader

`metadata::read_dimensions_fast(path)` dispatches by extension. Goal: _one_ file open per file regardless of which tier
handles it, because on a slow network share each open costs ~150 ms and that dominates everything else.

| Tier | Formats                    | Reader                                                                   | Why                                                                     |
| ---- | -------------------------- | ------------------------------------------------------------------------ | ----------------------------------------------------------------------- |
| 1    | Camera RAW                 | `rawler`, `raw_image(.., dummy = true)` + `raw_metadata`                 | Same crate that develops the file, so the crop rect matches             |
| 2    | PNG, GIF, BMP              | `image::image_dimensions`                                                | Tiny header, and no orientation the decode honours                      |
| 3    | JPEG (`jpg jpeg jpe jfif`) | open once → 64 KB buffer → `image` for dims + `nom-exif` for orientation | Both parsers run in-memory on the same buffer; single round trip        |
| 4    | WebP, TIFF, anything else  | the same buffered reader, format guessed from the magic bytes            | Sizes whatever the `image` crate opens, and answers `None` for the rest |

Tiers 3 and 4 are one function; only the routing differs, and tier 3 exists because the extension list is worth reading
off `decoding::dispatch` rather than matching by hand (`.jpe` and `.jfif` used to fall through to tier 4). 64 KB is well
above any JPEG's SOF + APP1/EXIF segments, typically both within the first 4 KB. When the prefix isn't enough — a TIFF
whose IFD sits at the end of the file, which plenty of writers produce — the reader pays a second open and lets both
parsers seek instead.

**Decision: every tier reads with the crate that decodes the format.**

**Why:** the number this returns is the number the window resizes to, so it has to be the number that eventually paints.
Sharing the decoder's parser makes that true by construction rather than by luck: the tier can only be wrong where the
decode is also wrong, and it answers `None` exactly where the decode would fail. Two concrete cases this settles:

- **RAW.** `decoding::raw` runs rawler's `CropActiveArea` and then its own `apply_default_crop`, so the develop ends on
  `crop_area`, else `active_area`, else the full sensor. `developed_raw_dimensions` states that rule in one pure
  function, and orientation comes from `raw_metadata(..).exif.orientation` — the same field `raw.rs` reads, because
  rawler hard-codes `RawImage.orientation` to `Normal`.
- **Orientation.** It comes from `nom-exif`, not from `image`'s own `ImageDecoder::orientation`, because `nom-exif` is
  what `decoding::orientation` runs on the real decode. The two disagree on WebP, whose EXIF chunk `image` reads and
  `nom-exif` doesn't. Following `image` there would have sized the window for a rotation the decode never applies.

This is also what let the module go cross-platform. It used to route RAW, WebP, TIFF, and every unknown extension to an
ImageIO (`CGImageSource`) tier, which is the entire reason `previews` was macOS-only. That FFI block is gone.

**Cost.** The RAW tier's `dummy` parse fills in geometry without allocating a pixel buffer or running any decompression:
179 µs warm per file on an M1 Max against roughly 1 ms for the ImageIO call it replaces (release build,
`tests/fixtures/raw/synthetic-bayer-128.dng`). The first RAW in a session also pays ~20 ms once, for rawler's bundled
camera database. That's a `lazy_static` shared with `decoding::raw_preview`, so a RAW open pays it either way; the tier
just reaches it first.

## Scheduler priority order

`scheduler::rebuild_queue` produces this order around `current`:

1. **Phase 1** — immediate neighbors (`dist 1..=PRELOAD_HALF`), centered outward. Most likely next nav target. Must have
   a placeholder ready _before_ the user presses arrow.
2. **Phase 2** — outside preload window (`dist > PRELOAD_HALF`), centered outward, capped by `WINDOW_RADIUS`.
   Exploration previews.
3. **Phase 3** — `current` itself, last. The primary decode is what we display; a preview for it is redundant.

The original order was Phase 2 → Phase 1 → current; that was wrong because a 7-image-per-second QL serving rate meant
immediate-neighbor previews arrived ~5 s after launch — leaving the user's first arrow-key press without a placeholder.
Swapping made the warm-up window for "the most likely nav target" go from ~5 s to <1 s.

## Preview size

`CGSize { 512, 512 }` at `NSScreen.backingScaleFactor` (2.0 Retina → 1024 effective pixels). This matches QuickLook's
gallery cache bucket, so folders the user has browsed in Finder's gallery view hit the cache instantly. Above 1024
effective, `quicklookd` renders from source every time — falls off the cache entirely.

## MCP

Exposed via the `previews_status` MCP tool. Returns folder length, current index, in-flight indices, queue length,
cached indices, failed indices, paused flag, and parallelism cap.

## Gotchas

- **Request rendered content only, never the icon representation.** The QL request uses
  `RepresentationTypes::Thumbnail | LowQualityThumbnail`, _not_ `All`. `All` lets quicklookd fall back to the generic
  file-type icon (the gray "DNG"/"RAF" document stamp) for files it can't render, which we'd then show full-window as a
  junk placeholder. Excluding `Icon` makes those files return an error instead (→ `PreviewFailed`, no placeholder), so
  the "Loading…" pill — and, for RAW, the embedded-JPEG preview — covers the gap. Don't switch back to `All`.
- **`QLThumbnailRepresentation::CGImage()` returns a `Retained<CGImage>` wrapper.** Pass its raw pointer via
  `Retained::as_ptr` to the FFI `CGContextDrawImage`; the `Retained` drops at end-of-scope and releases naturally.
- **Completion blocks run on quicklookd's queue.** `EventLoopProxy` is `Send`, so forwarding is fine, but don't touch
  `App` fields inside the block — the block exists only to post a user event. All mutation happens on the main thread.
- **Cancellation semantics.** We don't cancel individual in-flight requests mid-flight. On folder change,
  `RequestTable::cancel_all` fires `cancelRequest` on every live request. Individual cancellation isn't wired because
  the scheduler's queue-based model covers the common case.
- **`read_dimensions_fast` runs under `catch_unwind`, and it has to.** Header parsers assert on geometry that a corrupt
  file is free to lie about: `rawler` carries an outright `panic!` on absurd dimensions and asserts that the default
  crop nests inside the active area, and a `usize` underflow in border arithmetic panics in debug. This runs on the
  launch path and on 16 prefetch threads, so an unguarded panic would either take the process down or leave the pool a
  worker short for the rest of the session. `without_panicking` turns it into a `None` plus a debug log. Don't move the
  guard down into one tier: the contract is on the dispatcher.
- **A PNG `eXIf` chunk is the one orientation this tier can miss.** Tier 2 reads dimensions only, while
  `decoding::generic` runs `parse_exif_orientation` over the **whole** file, and `nom-exif` does parse a PNG `eXIf`
  chunk. So a PNG carrying a quarter turn auto-fits to the unrotated size and resizes once when the decode lands.
  Routing PNG through the buffered reader wouldn't fix it: `eXIf` is legal after `IDAT`, which puts it past the 64 KB
  prefix on any real photo, so a correct fix costs a second seeking pass over every PNG on the launch path. Not worth it
  for how rare the tag is on PNG. No other tier has the gap: a JPEG's APP1 sits right behind SOI, a TIFF keeps
  dimensions and orientation in the same IFD (so they succeed or fall back together), and neither this tier nor the
  decode reads WebP EXIF at all.
- **`little_exif` can't build a fixture for that.** `Metadata::write_to_file` picks the container from the extension and
  writes PNG EXIF as a **`zTXt`** chunk, which `nom-exif` doesn't read (it takes `eXIf` and an uncompressed `tEXt` "Raw
  profile type exif"). A round-trip through it looks like the gap is absent. Proving the case needs a hand-built `eXIf`
  chunk, CRC and all.

## File watcher integration (live folder sync)

The FSEvents watcher (`crate::folder_watch`) feeds previews two ways:

- **Add/remove (re-scan).** `apply_folder_rescan` calls `State::set_folder` again with the updated list — the scheduler
  cancels orphaned requests and enqueues new ones. `set_folder` is the single entry point for the path list.
- **Modify.** `State::forget_path(path)` drops the cached preview + scheduler `cached` entry + dim cache for that path
  so a later request regenerates it. quicklookd keys its own on-disk cache on file content/mtime, so a fresh request
  after the edit yields fresh pixels — we only need to evict OUR in-memory copy. Called from
  `App::handle_folder_changed` for each `Modify`-flagged path.

## Future work

- **Dedicated blur shader.** Current: linear-sampler softening on upscale. A separable Gaussian in a fragment shader
  would look nicer behind the "Loading..." overlay. Not required for correctness.
