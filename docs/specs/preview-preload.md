# Prvw: preview preload and display

Goal: when the user navigates to an image outside the preload window (for example, flips forward 3), show a blurry
preview + "Loading" overlay for the split second before the full decode lands, instead of a blank screen. Also:
background-generate previews for every file in the folder so quick mouse-wheel scrolls are covered.

## Principles

- **Respect disk.** Don't maintain our own on-disk preview store. Use the system-wide QuickLook cache, which is managed
  by `quicklookd` and shared with Finder, Preview, and every other Mac app.
- **Respect CPU.** Cap parallel preview requests (five at a time). Pause preview work while a primary decode is pending.
  Preview work is out-of-process (`quicklookd`) so the cap is mostly about I/O contention and system courtesy, not
  main-thread load.
- **Instant response.** Preview arrival must not race the primary decode in a way that flickers. If the full decode
  wins, the preview never shows.
- **Elegant simplicity.** Single scheduler struct. No dedicated thread. Results arrive via `EventLoopProxy` user events
  alongside existing nav events.

## Architecture

### APIs used

- **`QLThumbnailGenerator`** (`QuickLookPreviewing.framework`, macOS 10.15+) for preview requests. Block-based async;
  `quicklookd` handles generation, caching, and staleness (cache key includes the file's mtime, so modified files
  auto-invalidate).
- **`CGImageSourceCopyPropertiesAtIndex`** (`ImageIO.framework`) for reading pixel dimensions without decoding. Used by
  auto-fit-window so we know the final image size before the full decode lands.
- Rust binding: `objc2` + `objc2-quick-look-thumbnailing` + `objc2-image-io` (pin to current stable versions; do not
  trust model priors).

### Module layout

New module: `apps/desktop/src/previews/` with:

| File           | Purpose                                                                                                                          |
| -------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `mod.rs`       | `previews::State { scheduler, cache }`; public API: `set_folder`, `set_current`, `pause`, `resume`, `take_ready`                 |
| `scheduler.rs` | Queue ordering (centered traversal), parallelism cap, pause/resume, cancellation via retained request handles                    |
| `quicklook.rs` | `objc2` bridge to `QLThumbnailGenerator`; submits requests, forwards completions to main thread via `EventLoopProxy::send_event` |
| `metadata.rs`  | `ImageIO`-based pixel-dimension reader for auto-fit                                                                              |

`App` gains `previews: previews::State` alongside the existing per-feature states.

### Thread model

Main thread submits. Completion blocks forward results to main via
`EventLoopProxy::send_event(AppCommand::PreviewReady { index, image, dims })`. The scheduler state machine (queue,
in-flight map) lives on the main thread and is driven by:

- `AppCommand::NavigateDebounced` flush (already exists) → `previews::State::set_current(new_index)` reseeds queue
  order.
- `AppCommand::PreviewReady` → scheduler pulls next request from queue if under parallelism cap.
- Primary-decode start → `pause()`; primary-decode end → `resume()`.

Rejected alternative: a dedicated worker thread mirroring the preloader pattern. `QLThumbnailGenerator` is already async
out-of-process, so our thread would be a passthrough. Double-channel hop, extra state, no UX gain.

### Scheduler

State:

```rust
struct Scheduler {
    folder: Vec<PathBuf>,
    current: usize,
    queue: VecDeque<usize>,
    in_flight: HashMap<usize, RequestHandle>,
    max_parallel: usize,       // std::thread::available_parallelism() / 2, min 1
    paused: bool,
}
```

Queue order on `set_current(N)`: centered-outward, but indices inside the preload window (`N-2..=N+2`) go last because
the full-decode preloader will cover them anyway. So: `N+3, N-3, N+4, N-4, …` to the folder bounds, then
`N+2, N-2, N+1, N-1, N`. Non-wrapping at the ends. Every image in the folder gets a preview request eventually.

On every `set_current`, the queue is re-seeded: indices newly outside the preload window jump to the front, indices that
entered the window drop to the tail. Any in-flight request that's still relevant keeps going; in-flight requests for
nothing-relevant-anymore are left to complete (cancelling mid-flight has I/O cost and quicklookd is often near-done).

Parallelism cap: start up to `max_parallel - in_flight.len()` requests each time the scheduler ticks (on `set_current`,
on `PreviewReady`, on `resume`).

Pause semantics: `pause()` sets the flag; new requests don't start until `resume()`. In-flight requests keep running
(cancellation has I/O cost and quicklookd may already be most of the way done). Exception: on folder change, in-flight
requests for orphaned paths are cancelled via their retained handles.

Cancellation: `QLThumbnailGenerator.cancel(request)` takes a request handle. Store handles in `in_flight`.

### Preview request parameters

- Size: `CGSize { width: 512, height: 512 }`.
- Scale: `NSScreen.main.backingScaleFactor` (2.0 Retina, 1.0 external 1080p, future-proof for 3x).
- Representation types: `.all` (icon + lowQualityPreview + preview). We accept the first non-`.icon` rep for display and
  upgrade if a better one arrives later.

Why 512@scale: matches Finder's gallery/Cover Flow cache bucket, so folders the user browsed in gallery view hit cache
instantly. Above 1024 effective px, `quicklookd` renders from source each time (no bucket) — loses the cache benefit.

### ImageIO dimension read

For auto-fit-window to work when only the preview is loaded, we need the source pixel dimensions. Read via
`CGImageSourceCopyPropertiesAtIndex(source, 0, nil)` → `kCGImagePropertyPixelWidth` / `kCGImagePropertyPixelHeight`.
Does not decode pixels. ~1ms per file, works for RAW via ImageIO's camera support.

Strategy: lazy per-file on first access, cached in `previews::State` alongside the preview. Eager folder-load read is
tempting but folders can be 10k+ images and we'd do unnecessary I/O for files the user never reaches.

### RAW support

Free via `QLThumbnailGenerator` — `quicklookd` extracts the embedded JPEG preview (fast, ~10ms) from RAW EXIF, or falls
back to full ImageIO demosaic if no preview exists. Same code path, no special handling.

### Cache staleness

`quicklookd` keys its cache by file URL + mtime. Modified file → fresh preview generated automatically. We don't track
staleness ourselves. Free correctness.

## Display

### Render path

On cache miss (`navigation::State.pending_current = Some(index)`):

1. Read source pixel dimensions via ImageIO (cached). Resize window per auto-fit setting immediately — the preview _is_
   the image, just lower-res while loading, so the window should reach its final size before any pixels are shown.
2. Check `previews::State` for a ready preview for `index`.
3. If present: upload preview texture, render with Gaussian blur (wgpu fragment shader), render the loading overlay (see
   below) on top.
4. If absent: render blank background + loading overlay.
5. When `PreloadResponse::Ready` arrives for `pending_current`, swap to the full image texture, drop the preview texture
   and overlay.

### Loading overlay

Centered on screen. "Loading..." text rendered with the system font at a larger size than the window title text.
Rounded-rectangle backdrop matching the style of the existing title overlay at the top of the window, with a larger
corner radius proportional to the larger text size. Same text renderer pipeline as `render/text.rs`.

### Flicker policy

No delay threshold: the preview is the same image, just smaller and blurred, so there is no perceived flicker when it's
replaced by the full decode. Show preview (or blank + overlay if no preview yet) immediately on cache miss.

### Blur shader

Add a variant to `render/shader.wgsl` (or a new `render/blur.wgsl`) implementing a separable Gaussian blur with
`sigma ~ 8–12px` at display resolution. Single-pass approximation is fine; quality doesn't matter because the image is
transient. Decision postponed to implementation: separable two-pass blur is textbook but may be overkill for a 512px
source upscaled. A single-pass 9-tap box-blur downsample may look equally good on screen.

### Fallback

If `QLThumbnailGenerator` returns no representation (corrupt file, unsupported format, sandbox denial): render blank
background + loading overlay, same as the "no preview yet" path. When the full decode completes (or fails), the normal
path takes over.

## Settings

No new user-facing setting. The feature is always on. If we need a kill-switch for benchmarking, add a hidden flag in
`settings::general` later — not at launch.

## Priority interaction with existing preloader

The existing preloader (`navigation/preloader.rs`) runs serial on a dedicated `std::thread` for full decodes.
`quicklookd` is a separate process. They don't share CPU directly but they do share disk I/O and the overall system CPU.

Policy:

- Primary decode pending (`pending_current.is_some()`) → scheduler paused.
- Primary decode done → scheduler resumed.
- Preload window decodes (N±1, N±2) → scheduler runs alongside. They're not direct competitors and the user will benefit
  from previews being ready as they continue scrolling.

## Future: file watcher

Not in this spec. Design the scheduler's public API so `set_folder(paths)` is the only mutation point for the path list.
When we add `fsevents` later, it calls `set_folder` with the updated list; scheduler diffs, cancels orphaned requests,
enqueues new ones. No special-case code now.

## Out of scope

- On-disk preview store owned by prvw (we use the system cache).
- Preview display in any UI other than the blur-behind-loading-overlay (no grid view, no filmstrip — we're not a file
  manager).
- Preview sharing with Cmdr (Cmdr has its own preview strategy).
- Generating previews for non-image file types.

## Test plan

- Unit: scheduler queue ordering with various folder sizes, at start/middle/end of folder, with `max_parallel`
  saturated.
- Unit: pause/resume correctness (paused scheduler receives `set_current`, does not start requests until resumed).
- Unit: ImageIO dimension reader on a known-size JPEG, PNG, RAW fixture.
- Integration: navigate to index outside preload window → verify preview texture uploaded before full-decode ready
  event.
- Manual: browse a folder in Finder gallery view, then open prvw on the same folder, verify instant previews (cache
  hit).
- Manual: open a folder with RAW files never seen by Finder, verify previews appear within ~300ms.
- Manual: mash arrow keys across a 50-image folder, verify CPU stays reasonable and app remains responsive.
- QA server: new MCP tool `previews.status` returning scheduler state (folder size, in-flight indices, queue length,
  cached-preview indices, paused flag) for integration tests and debugging.
