# Navigation

Scan the parent directory for images, preload adjacent files in the background, and
keep an LRU cache budgeted at 512 MB.

| File           | Purpose                                                                                    |
| -------------- | ------------------------------------------------------------------------------------------ |
| `mod.rs`       | `navigation::State { dir_list, preloader, image_cache, history, current_image_size }`      |
| `directory.rs` | `DirectoryList` — scan parent dir for supported extensions, sort, track current position   |
| `preloader.rs` | Rayon thread pool + `ImageCache` with LRU eviction (512 MB budget)                         |

## State

`App.navigation: navigation::State` owns this feature's runtime. Note the `history`
field holds `VecDeque<NavigationRecord>` — the type is defined in `crate::diagnostics`
(it's a measurement record). Navigation pushes entries; diagnostics formats them.

## Key patterns

- **`std::thread` + channels, no `tokio`.** Preloader uses rayon (`min(4, cores-1)`
  threads) for CPU-bound decoding. Results come back via `std::sync::mpsc`. An
  in-flight `HashSet` prevents duplicate work.
- **Cancellation.** Preload tasks hold an `Arc<AtomicBool>`; navigation away cancels
  in-flight decodes so they don't block newer work.
- **Supported extensions are decided by `decoding`** — `DirectoryList` filters via
  `decoding::is_supported_extension`. New format support = one change, two effects
  (decode + list).

## Gotchas

- **`zune-jpeg` in debug builds.** Its SIMD is painfully slow without optimizations.
  `Cargo.toml` sets `[profile.dev.package.zune-jpeg] opt-level = 3`.
