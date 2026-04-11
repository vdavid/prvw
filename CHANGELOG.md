# Changelog

All notable changes to Prvw are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning: [Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-04-11

### Added

- JPEG decoding via `zune-jpeg` with SIMD acceleration (NEON on Apple Silicon, AVX on x86), ~27% faster than the
  `image` crate's built-in decoder ([2e67fd3](https://github.com/vdavid/prvw/commit/2e67fd3))
- Parallel preloading with rayon thread pool (uses all available cores instead of a single thread), ~2-3x faster for
  NAS browsing ([2e67fd3](https://github.com/vdavid/prvw/commit/2e67fd3))
- Priority preloading with cancellation tokens — navigating cancels stale preloads, current image gets priority via
  `spawn_fifo`, chunked file reads (64 KB) allow sub-2ms cancellation on NAS
  ([68dbe31](https://github.com/vdavid/prvw/commit/68dbe31))
- EXIF orientation support via `nom-exif` — phone photos (portrait orientation 6/8) now display right-side-up
  ([d2d95bc](https://github.com/vdavid/prvw/commit/d2d95bc))
- Embedded MCP server (streamable HTTP on port 19447) for agent testing and E2E — tools: navigate, key, zoom,
  fullscreen, open, screenshot; resources: state, menu, diagnostics
  ([c7f4875](https://github.com/vdavid/prvw/commit/c7f4875),
  [3751813](https://github.com/vdavid/prvw/commit/3751813))
- Performance diagnostics via MCP and HTTP — cache state with per-image decode times, navigation history with timing,
  process RSS ([3751813](https://github.com/vdavid/prvw/commit/3751813))
- Cmdr-style logging with timestamps, colored log levels, and short module scopes. `RUST_LOG=debug` shows the full
  decode/preload/navigation flow ([ca94104](https://github.com/vdavid/prvw/commit/ca94104))
- JPEG decode benchmark (zune-jpeg vs turbojpeg, 20 Pixel photos). Key finding: NAS I/O is the bottleneck, not CPU
  decode ([1956496](https://github.com/vdavid/prvw/commit/1956496))

### Changed

- Window title format: `3 / 60 – photo.jpg` (position first for quick scanning), loading state: `3 / 60 – Loading...`
  ([7509317](https://github.com/vdavid/prvw/commit/7509317))

### Fixed

- Crash on navigation (Left/Right arrow) — muda 0.17 panics with `ZeroWidth` icon error when processing keyboard
  accelerators on macOS. All accelerators removed from menu items, shortcuts handled directly in keyboard event handler
  ([5aa98e8](https://github.com/vdavid/prvw/commit/5aa98e8))
- Fullscreen on/off via QA server now uses `set_fullscreen` directly instead of toggling
  ([e34b0f8](https://github.com/vdavid/prvw/commit/e34b0f8))

## [0.1.0] - 2026-04-11

### Added

- Initial release: GPU-accelerated image viewer for macOS
- `winit` 0.30 windowing with `ApplicationHandler` trait, `wgpu` 29 Metal rendering, `muda` 0.17 native menus
- Image formats: JPEG, PNG, GIF (first frame), WebP, BMP, TIFF
- Zoom and pan: scroll wheel (cursor-centered), click-drag, keyboard shortcuts (+/-/0), double-click toggle
  fit-to-window vs actual size
- Directory navigation: Left/Right arrows, background preloading of adjacent images (N±2)
- Fullscreen toggle (Cmd+F, F11), ESC to exit fullscreen or close
- Native macOS menu bar: File, View (zoom, fullscreen), Navigate (prev/next)
- Render-on-demand (no continuous GPU loop when idle)
- getprvw.com marketing website (Astro + Tailwind v4), sky blue brand palette
- Go check runner (14 checks: Rust, Astro, Go) with parallel execution and dependency graph
- GitHub Actions CI (ubuntu + macOS runners, change detection)
- Full docs: AGENTS.md, CONTRIBUTING.md, architecture, design principles, style guide
