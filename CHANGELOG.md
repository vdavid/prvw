# Changelog

All notable changes to Prvw are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning: [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Auto-fit window: window resizes to match each loaded image, centered on screen. Clamped to min 200px, max 90% of
  monitor, proportionally scaled. Toggle in View menu and Settings window
  ([6a8e03d](https://github.com/vdavid/prvw/commit/6a8e03d))
- Auto-fit zoom: when auto-fit is on, zooming in/out resizes the window to match the zoomed image. Cursor pivot stays
  fixed when growing, symmetric shrink when reducing. Screen boundary preserved
  ([6c4764f](https://github.com/vdavid/prvw/commit/6c4764f))
- Enlarge small images toggle (off by default): small images display at native pixel size instead of being stretched to
  fill the window. Toggle in View menu and Settings window, disabled when auto-fit is on
  ([c2c73c8](https://github.com/vdavid/prvw/commit/c2c73c8))
- Checkerboard background for transparent images (Photoshop-style, screen-space so it doesn't zoom)
  ([d481774](https://github.com/vdavid/prvw/commit/d481774))
- Custom overlay text with pill backgrounds: SF Pro system font (bold, 13.5pt), semi-transparent rounded rectangles
  sized from actual measured text width, middle truncation for long filenames (`prefix…suffix`), right-aligned zoom
  percentage. Native title bar text hidden
  ([d0006fc](https://github.com/vdavid/prvw/commit/d0006fc))
- Native AppKit windows for About (with icon, links), Settings (toggles with live apply), and Onboarding (file
  association setup with live polling). Frosted glass backgrounds, ESC-to-close, deduplication guard
  ([644132b](https://github.com/vdavid/prvw/commit/644132b))
- Settings persistence with `PRVW_DATA_DIR` env var override for dev/test isolation
- View > Refresh menu item (R key)
- MCP server improvements: JSON state responses, synchronous command completion, `prvw://settings` resource,
  `set_window_geometry`, `scroll_zoom`, `zoom_in`/`zoom_out` tools, window position in state
  ([593cac9](https://github.com/vdavid/prvw/commit/593cac9),
  [c2c73c8](https://github.com/vdavid/prvw/commit/c2c73c8))

### Changed

- Zoom model: now absolute (1.0 = one image pixel per screen pixel). Zoom % in titlebar shows actual pixel scale.
  Enables auto-fit zoom without feedback loops
  ([3b2f51e](https://github.com/vdavid/prvw/commit/3b2f51e))
- Scroll zoom slowed to 5% per tick (was 15%)
  ([d2ce180](https://github.com/vdavid/prvw/commit/d2ce180))
- Input handling unified through `AppCommand`: all keyboard, menu, and QA key events mapped in one place (`input.rs`).
  Central `execute_command()` handler
  ([4dbf326](https://github.com/vdavid/prvw/commit/4dbf326))
- Background color changed from dark gray to black
- Settings window buttons changed from "OK" to "Close" (toggles apply immediately)
- File association setup uses direct CoreServices FFI instead of `swift -e` scripts (near-instant)

## [0.5.0] - 2026-04-12

### Added

- Text rendering via glyphon (wgpu-native, cross-platform)
- Onboarding screen when launched with no file: shows welcome, file association status, "Set as default viewer"
- Header overlay in image view: filename, position, zoom level in the transparent title bar
- Transparent titlebar with frosted glass effect on macOS (fullSizeContentView)
- LSHandlerRank changed to Default (Prvw appears higher in "Open With" menus)
- Styled DMG installer (icon positioning, window sizing via create-dmg)

## [0.4.0] - 2026-04-12

### Added

- Auto-updater: background update check, DMG download, .app bundle replacement. Restart to use the new version.
  PRVW_UPDATE_URL env var override for testing.
- Direct download buttons on website with architecture detection (Apple Silicon / Intel / Universal)
- PostHog session replay (cookieless, proxied through /ph/)
- Umami download tracking with arch, version, and source properties
- Sitemap via @astrojs/sitemap
- UptimeRobot monitoring for getprvw.com
- Terms and conditions and privacy policy pages
- Deploy infrastructure: webhook, CI auto-deploy on push to main

### Fixed

- Download dropdown not opening (used DOMContentLoaded instead of astro:page-load)
- Updater: fix .app replacement (fs::rename over non-empty dir fails on macOS)

## [0.3.0] - 2026-04-12

### Added

- Multiple file args: `prvw photo1.jpg photo2.jpg` uses the provided files as the navigation set instead of scanning
  the directory. Supports multi-select "Open With" from Finder
  ([c49761d](https://github.com/vdavid/prvw/commit/c49761d))
- Keyboard shortcuts: Space/] for next, Backspace/[ for previous, F/Enter for fullscreen, 1 for actual size
  ([f0c24f8](https://github.com/vdavid/prvw/commit/f0c24f8))
- Clickable menu items: About Prvw, View (zoom, fit, actual size, fullscreen), Navigate (prev/next). Fixed root cause
  (Menu object must be kept alive to prevent dangling pointer in NSMenuItems)
  ([7e9d0dd](https://github.com/vdavid/prvw/commit/7e9d0dd))
- About dialog showing version, author, and website links
- macOS window config: disabled system tab bar and native fullscreen (we have our own borderless fullscreen)
- Poll menu events in `about_to_wait` for instant response (was delayed until next window event)
- macOS .app bundle with Info.plist, file type associations (JPEG, PNG, GIF, WebP, BMP, TIFF), app icon
- Apple Events handler via NSAppleEventManager for opening files while app is running
- Release infrastructure: GitHub Actions workflow, signing, DMG creation, notarization
- Root Cargo workspace (matching Cmdr's structure)

### Fixed

- Aspect ratio always preserved during window resize (rewrote view transform with single uniform scale)
- Zoom: can't zoom out past fit-to-window, zoom pivot correct after resize
- Pan clamped to image edges, re-clamped on window resize
- Blank startup: retry render when wgpu surface is Occluded during window creation
- CI: install libglib2.0-dev + libgtk-3-dev for winit on Ubuntu

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
