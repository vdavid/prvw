# Previews

Two halves that share a folder list, with different reach:

- **Dimensions, everywhere.** Read `(width, height)` from a file's header without decoding it, so the window can
  auto-fit to the final image size before the first pixel paints. Runs on macOS, Windows, and Linux.
- **Preview pixels, macOS and Windows.** Background-generate previews for every file in the current folder so navigating
  to an image outside the full-decode preload window shows a blurry placeholder instantly instead of a blank screen.
  macOS submits to the system-wide QuickLook cache (`quicklookd`), shared with Finder, Preview, and every other Mac app.
  Windows runs `generator.rs`, a worker pool that reads the shell's thumbnail cache where it can and decodes where it
  can't. Linux has no generator, so its cache stays empty and every caller falls through to the dimension half.

| File              | Purpose                                                                                                          | Platforms      |
| ----------------- | ---------------------------------------------------------------------------------------------------------------- | -------------- |
| `mod.rs`          | `State { scheduler, cache, dim_prefetcher, paths, current, requests }` + API + RAM-scaled byte budget + eviction | all            |
| `metadata.rs`     | Four-tier dim+orientation reader (rawler / image / image+nom-exif)                                               | all            |
| `dim_prefetch.rs` | 16-thread parallel pool that pre-warms `(width, height)` for window indices                                      | all            |
| `scheduler.rs`    | Pure state machine: priority-ordered queue, windowing, parallelism cap, pause/resume                             | macOS, Windows |
| `request.rs`      | What a request carries and what comes back: `SubmitRequest`, `Delivery`, the pending queue and its wake rule     | all            |
| `generator.rs`    | Prvw's own generator: route decision, worker pool, downscaling, the DIB fixup                                    | all (compiled) |
| `quicklook.rs`    | QL submission worker thread + `Retained<...>` lifecycle + `CGImage → RGBA8` blit                                 | macOS          |
| `shell.rs`        | `IShellItemImageFactory` + the COM apartment + `HBITMAP → RGBA8`                                                 | Windows        |

`previews::RequestTable` is whichever of the two a platform has, so everything above it — `State`, `App`'s pump, the
`PreviewsAvailable` arm of `execute_command` — is one code path. `generator.rs` compiles on every host on purpose: its
routing, sizing, and pixel-layout decisions are Windows behaviour that a Mac can assert, which is the only way any of it
gets checked before meeting a Windows box. Off Windows it carries a module-level dead-code allow, the same shape
`mod parity;` uses, and `mod previews;` in `main.rs` carries one for Linux.

## Flow

1. `App::resumed` calls `State::set_folder(paths, current)` after the directory scan.
2. The scheduler enqueues every folder index, ordered centered-outward but with indices inside the full-decode preload
   window (`|i − current| ≤ 2`) pushed last — the full-decode preloader will cover those anyway, so they're the
   lowest-value previews to fetch.
3. `App::pump_preview_requests` drains the scheduler up to `previews::max_parallel()` (`available_parallelism() / 2`,
   min 1) and submits each to the platform's `RequestTable`.
4. The generator produces a 512 pt × scale preview: `quicklookd` on macOS, a pool worker on Windows.
5. The result is pushed onto the shared pending queue, which fires `AppCommand::PreviewsAvailable` via `EventLoopProxy`
   **only when the queue was empty**, so a burst of completions costs one or two `user_event`s rather than one each.
6. `App::execute_command` drains the queue, stores each preview in the cache and — if one is for `pending_current` —
   uploads it into the image texture as a placeholder. The full decode later replaces it.

## Key patterns

- **System cache where there is one, ours where there isn't.** `quicklookd` keeps an on-disk cache keyed by file URL +
  mtime, and Windows' shell keeps `thumbcache_*.db`; modified files auto-invalidate in both, which means we never check
  staleness ourselves. Disk cost: zero; cache hygiene: OS-managed. What Windows can't get that way (RAW, and any path
  with no legal shell spelling) it decodes, and nothing is cached to disk for those.
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

## Gotcha: a folder change has to free the in-flight slots

`Scheduler::set_folder` clears `in_flight` along with the cache and the queue, and it has to. Whatever those requests
eventually deliver arrives stamped with the old `folder_generation` and `execute_command` drops it before it can reach
`mark_ready` or `mark_failed`, so leaving the entries behind leaks up to `max_parallel` slots per folder change. Once
the map is full, `poll_next` answers `None` forever and previews go quiet for the rest of the session. The indices are
the old folder's anyway, and mean nothing in the new one.
`scheduler::tests::a_folder_change_frees_the_in_flight_slots` holds the line.

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

## QL submission threading (macOS)

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

## The Windows generator

`generator.rs` picks one of three routes per file (`route_for`, pure and unit-tested from any host), then runs it on a
`previews::max_parallel()`-thread pool. `shell.rs` is the only Windows-only file; everything else in the generator is
portable Rust.

| Route         | Files                                                         | How                                                                     |
| ------------- | ------------------------------------------------------------- | ----------------------------------------------------------------------- |
| `EmbeddedRaw` | every camera RAW                                              | `decoding::decode_raw_preview` — the camera's embedded JPEG, no develop |
| `System`      | everything else with a legal shell path (`paths::shell_path`) | `IShellItemImageFactory::GetImage`, the cache Explorer fills            |
| `Decode`      | everything else                                               | `decoding::load_image` + a box-filter downscale                         |

**Decision: RAW never goes to the shell.** **Why:** the shell only renders a RAW when Microsoft's Raw Image Extension is
installed, so without it every RAW in the folder is a blank screen; and with it, Microsoft's develop isn't Prvw's, so
the placeholder would visibly shift colour when the real develop landed. `decode_raw_preview` is the same call
`navigation::preloader` already makes on a RAW cache-miss, so the preview and the quick preview that follows it are the
same pixels rather than two guesses. Cheap, too: an embedded-JPEG decode, no develop.

**Decision: a path the shell can't take falls back to decoding, not to nothing.** **Why:** `shell_path` answers `None`
for a path that would be mangled once de-verbatimed (over `MAX_PATH`, a volume-GUID path, a reserved DOS device name),
and deep libraries on a NAS are exactly the users Prvw is built for. Losing previews for them would be the wrong way to
save the work.

**Decision: previews are sRGB on every route and every platform.** **Why:** the shell hands back sRGB-ish pixels with no
way to ask for anything else, and quicklookd is no more display-managed. Colour-managing only the RAW route would leave
one route disagreeing with the other two on a wide-gamut display. The colour-managed full decode replaces the
placeholder within a second either way.

**Decision: a pool, where macOS has one thread.** **Why:** `quicklookd` is out-of-process and asynchronous, so one
submitting thread is enough there; every route here is synchronous and occupies the thread running it. The pool is
`max_parallel()` threads, the same number the scheduler will let be in flight, so a queued job always has a worker. Note
the cap's _reason_ differs per platform even though the number doesn't: on macOS it's I/O and system courtesy, on
Windows it's leaving cores for the full decode of the image the user is actually looking at.

**Cancellation** is an epoch counter rather than a protocol: `cancel_all` bumps it, and a worker drops any job whose
stamp no longer matches, before the file is touched and again after. A job already running finishes — a decode has no
checkpoint and a shell call is someone else's to abort — and its delivery is dropped by `execute_command` for carrying a
stale `folder_generation`.

### Windows gotchas

- **`SIIGBF_THUMBNAILONLY`, never the icon.** Same trap as the QuickLook `RepresentationTypes` gotcha below: without it
  the shell returns the file type's generic icon for anything it can't render, and the app would blow that up to fill
  the window.
- **Never `SIIGBF_SCALEUP`.** The requested `SIZE` is a box the thumbnail is fitted into with its aspect kept; `SCALEUP`
  turns that into "always fill the box" and pads with transparent margins. The placeholder is drawn against source
  dimensions read from the file's header, so padding would show as an off-centre, wrongly-scaled image.
- **The alpha byte is only meaningful when it's non-zero somewhere.** A thumbnail composited by a GDI path that predates
  alpha comes back with every alpha byte zero, which uploads as a fully transparent placeholder. `dib_to_rgba8` reads an
  all-zero alpha channel as "no alpha here" and forces it opaque, and takes any non-zero byte at face value.
- **Every worker enters a single-threaded apartment.** Shell thumbnail providers are registered apartment-threaded; an
  MTA caller has COM marshal each call into one host STA and serialise the whole pool behind it. A synchronous outbound
  COM call from an STA runs a modal message loop inside the call, which is exactly why this can only happen on a worker
  — that loop on the event-loop thread is `AGENTS.md`'s starved-pump failure. `RPC_E_CHANGED_MODE` means someone got
  there first, and then the guard must not call `CoUninitialize`.

**Never executed on Windows.** Every line above is compile-verified only (`--check windows-cross`), like the rest of the
Windows port.

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

512 points on the longest edge, at the window's scale factor (2.0 Retina → 1024 effective pixels). A request carries the
point size and the scale **separately**, because `QLThumbnailGenerationRequest` keys quicklookd's cache on the pair: 512
pt at scale 2 hits the gallery bucket Finder fills, and 1024 pt at scale 1 misses it entirely and re-renders from source
every time. `generator::request_pixels` multiplies them for the platforms that ask in pixels. Windows has no bucketing
to match, and 512 pt is a good number there too: enough to fill a 4K window softly, few enough that the byte budget
holds a useful neighbourhood.

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
  so a later request regenerates it. quicklookd and the Windows shell both key their on-disk caches on file
  content/mtime, so a fresh request after the edit yields fresh pixels — we only need to evict OUR in-memory copy.
  Called from `App::handle_folder_changed` for each `Modify`-flagged path.

## Future work

- **Dedicated blur shader.** Current: linear-sampler softening on upscale. A separable Gaussian in a fragment shader
  would look nicer behind the "Loading..." overlay. Not required for correctness.
