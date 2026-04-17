# Changelog

All notable changes to Prvw are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning: [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Camera RAW support via `rawler`: open DNG, CR2, CR3, NEF, ARW, ORF, RAF, RW2, PEF, and SRW files. Decode pipeline
  includes black/white level correction, PPG demosaic for Bayer sensors, bilinear for Fuji X-Trans, white balance,
  camera color matrix with Bradford chromatic adaptation, and sRGB gamma. NEON SIMD on Apple Silicon, rayon
  parallelism. Orientation is pulled from EXIF metadata since `rawler` hard-codes `RawImage.orientation`. Known limits
  in this first pass: no DNG `OpcodeList` interpretation (iPhone ProRAW gain maps), no DCP profiles, X-Trans demosaic
  is bilinear. See `docs/notes/raw-support-phase1.md` for design decisions and the Phase 2/3 outlook
  ([b4bc775](https://github.com/vdavid/prvw/commit/b4bc775))
- File associations for all 10 RAW formats: Finder now recognizes Prvw as a handler for DNG, CR2, CR3, NEF, ARW, ORF,
  RAF, RW2, PEF, and SRW via their standard Apple UTIs. `Info.plist` carries all 16 document types now (6 standard + 10
  RAW)
- Settings > File associations: redesigned into two sections, "Standard image formats" (JPEG, PNG, GIF, WebP, BMP,
  TIFF) and "Camera RAW formats" (DNG, CR2 + CR3, NEF, ARW, ORF, RAF, RW2, PEF, SRW) with vendor labels. Each section
  has a master "Set all" toggle with tri-state support: when some formats are on and others off, the master shows a
  "Mixed" indicator; clicking mixed or off sets all on, clicking on sets all off. Per-format small toggles keep fine
  control

### Changed

- Decoding module: single-file `decoding.rs` split into a `decoding/` module with per-backend files (`jpeg.rs`,
  `generic.rs`, `raw.rs`) plus shared `dispatch.rs` and `orientation.rs`. Public API unchanged
  ([b4bc775](https://github.com/vdavid/prvw/commit/b4bc775))
- CI: macOS-only modules (AppKit settings panels, color transform tests) gated behind `#[cfg(target_os = "macos")]`
  so cross-platform builds compile cleanly. Groundwork for Windows and Linux support later
  ([e9b5de4](https://github.com/vdavid/prvw/commit/e9b5de4),
  [3f00979](https://github.com/vdavid/prvw/commit/3f00979),
  [815b727](https://github.com/vdavid/prvw/commit/815b727),
  [96218dd](https://github.com/vdavid/prvw/commit/96218dd))

### Fixed

- `apply_orientation` underflowed on zero-width or zero-height input for EXIF orientation 2. Now early-returns
  ([b4bc775](https://github.com/vdavid/prvw/commit/b4bc775))

## [0.8.0] - 2026-04-17

### Added

- Settings window: new sidebar layout with General, Zoom, Color, and File associations sections. Cross-dependencies
  disable dependent toggles automatically (ICC off → Color match / Relative colorimetric disabled; Auto-fit on →
  Enlarge disabled) ([dc43505](https://github.com/vdavid/prvw/commit/dc43505),
  [0dd4849](https://github.com/vdavid/prvw/commit/0dd4849))
- File associations panel: per-UTI toggles, "Set all" master toggle, 1-second polling of handler state, previous
  handler rollback when you turn a toggle off ([0dd4849](https://github.com/vdavid/prvw/commit/0dd4849),
  [17b76a3](https://github.com/vdavid/prvw/commit/17b76a3))
- Rendering intent toggle (View menu + Settings > Color, Cmd+Shift+R). Default is perceptual; toggle to relative
  colorimetric. Disabled when ICC color management is off. Persisted as `use_relative_colorimetric`
  ([b42814f](https://github.com/vdavid/prvw/commit/b42814f))
- Scroll-to-zoom toggle in Settings > General (off by default). When off, scroll navigates between images (down = next,
  up = prev). Cmd+scroll always zooms regardless of the setting
  ([d55b7e9](https://github.com/vdavid/prvw/commit/d55b7e9))
- Pinch-to-zoom on trackpad, cursor-centered. Works with auto-fit window resize, same as scroll zoom
  ([ef8d0bf](https://github.com/vdavid/prvw/commit/ef8d0bf))
- Keyboard shortcuts for zoom: Cmd+= (zoom in), Cmd+- (zoom out), Cmd+0 (actual size)
  ([ec2aba4](https://github.com/vdavid/prvw/commit/ec2aba4))
- Title bar toggle in Settings > General (on by default): reserves a 32px strip at the top so the filename and zoom
  pills don't cover the image. Screenshot-friendly when off
  ([64e0d87](https://github.com/vdavid/prvw/commit/64e0d87))
- Title bar vibrancy: Liquid Glass on macOS 26, classic frosted glass on older versions. The area around the image
  (when the image doesn't fill the window) gets a darker HUD-style vibrancy. Fullscreen switches both to opaque black
  so screenshots and projector-style viewing aren't distracted by the desktop blurring through
  ([7eede14](https://github.com/vdavid/prvw/commit/7eede14))
- Integration test suite (17 tests): Settings open/close/switch, zoom in/out, fit/actual, auto-fit toggle, navigate,
  refresh, window geometry. Each test spawns its own app instance on a dynamic port
  ([0dd4849](https://github.com/vdavid/prvw/commit/0dd4849))

### Changed

- Source layout: flat `src/` with infrastructure (`app/`, `render/`, `platform/`) and features (`about`, `color`,
  `decoding`, `navigation`, `onboarding`, `qa`, `settings`, `window`, `zoom`, …) as siblings. Each feature owns its
  runtime state via a `State` struct on `App` instead of ~20 flat fields. No behavior change
  ([27eca5e](https://github.com/vdavid/prvw/commit/27eca5e),
  [e88027b](https://github.com/vdavid/prvw/commit/e88027b))

### Fixed

- Closing the onboarding window now quits the app. Previously, a no-file launch (Dock or `cargo run` with no args)
  left the event loop running with nothing visible after the user clicked Close, because the onboarding is a raw
  AppKit window and doesn't generate a winit close event
  ([e81bbdf](https://github.com/vdavid/prvw/commit/e81bbdf))
- CGColor / CGColorSpace encoding crashes in `setColorspace:` (display profile) and the Settings separator:
  `msg_send!` encoded these as `^v` instead of `^{CGColorSpace=}` / `^{CGColor=}`. Fix uses raw `objc_msgSend` to
  bypass the type check ([17b76a3](https://github.com/vdavid/prvw/commit/17b76a3))

## [0.7.0] - 2026-04-16

### Added

- ICC color management: embedded source profiles (JPEG, PNG, TIFF, WebP) are transformed to accurate output colors.
  Level 1 converts to sRGB, Level 2 targets the actual display profile via CoreGraphics FFI
  (`CGDisplayCopyColorSpace`). Images without profiles assumed sRGB. Display changes flush the cache and re-decode
  ([ee226ac](https://github.com/vdavid/prvw/commit/ee226ac),
  [94820a8](https://github.com/vdavid/prvw/commit/94820a8))
- View menu toggles: "ICC color management" (Cmd+Shift+I) and "Color match display" (Cmd+Shift+C), both persisted in
  settings. Disabling ICC grays out color matching (L2 depends on L1)
  ([a088330](https://github.com/vdavid/prvw/commit/a088330),
  [b952b64](https://github.com/vdavid/prvw/commit/b952b64))

### Changed

- ICC engine: replaced lcms2 (C bindings) with moxcms (pure Rust, NEON SIMD). 24MP transform: 247ms -> 45ms on M3 Max.
  No C toolchain needed for cross-compilation
  ([f568b18](https://github.com/vdavid/prvw/commit/f568b18))

### Fixed

- Screen detection: replaced unreliable `current_monitor()` + `CGDisplayBounds` position matching with
  `NSWindow.screen.deviceDescription` for authoritative `CGDirectDisplayID`
  ([fcdefe3](https://github.com/vdavid/prvw/commit/fcdefe3))
- Pre-existing BGRA->RGBA swap bug in screenshot capture
  ([ee226ac](https://github.com/vdavid/prvw/commit/ee226ac))

## [0.6.3] - 2026-04-15

### Fixed

- Finder double-click file opening: replaced `NSAppleEventManager` handler (overridden by AppKit's
  `NSDocumentController`) with ObjC runtime method injection (`class_addMethod`) that adds
  `application:openURLs:` directly to winit's `WinitApplicationDelegate` class
  ([9417ab0](https://github.com/vdavid/prvw/commit/9417ab0))

### Changed

- Zoom model uses logical pixels: zoom=1.0 = 100% = one image pixel per logical pixel. The overlay
  correctly shows 100% for naturally-sized images on Retina displays (was 200%)
- Compiler-enforced `Logical<T>` / `Physical<T>` newtypes prevent mixing logical and physical pixel
  values. Winit interop via `from_logical_size`, `to_logical_pos`, etc.
- Removed 329 lines of dead modal onboarding code

## [0.6.2] - 2026-04-15

### Fixed

- Finder double-click "cannot open files in JPEG Image format": `CFBundleTypeRole` was missing from `Info.plist`
  document type entries. macOS requires this to know the app can actually open files, not just be registered as a handler
- CI: add `libxdo-dev` to Linux apt-get for `muda` dependency
- Auto-updater: call `lsregister -f` after replacing the `.app` bundle so macOS picks up new document types in future
  updates

## [0.6.1] - 2026-04-15

### Fixed

- Finder double-click now works: macOS sends file paths via Apple Events (not CLI args), but the app was exiting before
  the event loop started. Now the event loop always runs, with a 500ms wait for Apple Events before showing onboarding
  ([f6e0fef](https://github.com/vdavid/prvw/commit/f6e0fef))

### Changed

- Onboarding window is now non-modal (doesn't block the event loop), allowing Apple Events and QA commands to arrive
  while it's showing
- Code refactors: `scale_factor` stored on App, `TextBlock` builder pattern, `MonitorBounds` helper, `LogicalF64` /
  `LogicalF32` type aliases for coordinate clarity

## [0.6.0] - 2026-04-15

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
