# Navigation

Scan the parent directory for images, preload adjacent files in the background, and
keep an LRU cache budgeted at 512 MB (SDR) or 1 GB (HDR, Phase 5). The cache
auto-scales when the RAW pipeline's `hdr_output` flag flips or the display's EDR
headroom crosses the 1.0 boundary, so preload count stays constant as we double
per-pixel bytes for RAW RGBA16F.

| File           | Purpose                                                                                    |
| -------------- | ------------------------------------------------------------------------------------------ |
| `mod.rs`       | `navigation::State { dir_list, preloader, image_cache, history, current_image_size, preload_neighbors, pending_current }` |
| `directory.rs` | `DirectoryList` — scan parent dir for supported extensions, sort, track current position   |
| `preloader.rs` | Rayon thread pool + `ImageCache` with LRU eviction (512 MB budget)                         |

## State

`App.navigation: navigation::State` owns this feature's runtime. Note the `history`
field holds `VecDeque<NavigationRecord>` — the type is defined in `crate::diagnostics`
(it's a measurement record). Navigation pushes entries; diagnostics formats them.

## Navigation render path

On cache hit, `navigate` renders from cache synchronously and submits neighbor
preloads. On cache miss it sets `State.pending_current = Some(index)`, shows a
"Loading…" title, and submits the target as the priority-zero preload task
(first entry in `request_preload`'s `tasks` list → FIFO slot). `poll_preloader`
runs the render when `PreloadResponse::Ready { index }` matches
`pending_current`, then clears it. The main thread never decodes navigation
targets directly — only settings re-decode and `Refresh` still call the sync
`display_image` path.

## Key patterns

- **`std::thread` + channels, no `tokio`.** Preloader uses rayon (`min(4, cores-1)`
  threads) for CPU-bound decoding. Results come back via `std::sync::mpsc`. An
  in-flight `HashSet` prevents duplicate work.
- **Cancellation.** Preload tasks hold an `Arc<AtomicBool>`; navigation away cancels
  in-flight decodes so they don't block newer work.
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
