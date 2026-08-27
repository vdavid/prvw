# Architecture

High-level map of Prvw's components. Each module has a colocated `CLAUDE.md` (or `//!` module docs for single-file
features); this page is the index.

## Desktop app (`apps/desktop/`)

Pure Rust (`winit` + `wgpu` + `muda`). Flat `src/` layout: infrastructure and features sit as siblings. `App` holds
per-feature state via `zoom::State`, `color::State`, `navigation::State`.

### Source layout (`src/`)

**Infrastructure:**

| Path                        | Role                                                                                                  |
| --------------------------- | ----------------------------------------------------------------------------------------------------- |
| `main.rs`                   | Thin entry: CLI, logger, event-loop setup                                                             |
| `app.rs` + `app/`           | `App`, `ApplicationHandler`, command dispatcher, `SharedAppState`                                     |
| `chrome.rs`                 | What colour every Win32 window of ours paints: theme, surface, ink, and the high-contrast rule        |
| `clipboard.rs`              | The byte layouts Windows' clipboard formats want (`CF_DIB`, `CF_DIBV5`, `CF_HDROP`)                   |
| `commands.rs`               | `AppCommand` enum + global `EventLoopProxy`                                                           |
| `folder_watch.rs`           | Live folder sync: `notify` FSEvents watcher + pure debounce/coalesce + off-thread re-scan lister      |
| `input.rs`                  | Maps keys and QA keys to `AppCommand`                                                                 |
| `launch.rs`                 | What Prvw is asked to open, by argument or by drop: wait for a file, a folder's images, nothing       |
| `logging.rs`                | `env_logger` setup, and where a console-less Windows launch writes instead                            |
| `menu/`                     | Menu bar and context menu via `muda` (macOS, Windows), and the seam for platforms with neither        |
| `paths.rs`                  | What "the same path" means per platform: verbatim `\\?\` prefixes, case folding, the display boundary |
| `pixels.rs`                 | `Logical` / `Physical` newtypes for coordinate types                                                  |
| `platform.rs` + `platform/` | Cross-cutting platform glue (Apple Events, AppKit helpers, both clipboards, the Windows console)      |
| `render.rs` + `render/`     | wgpu infrastructure: renderer, text overlay, shaders                                                  |
| `scroll.rs`                 | What a wheel notch or a trackpad swipe means, per platform: zoom modifier, zoom steps, images         |

**Features:**

| Path                 | Owns                                                                                                                                                            |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `about/`             | About window: shared copy in `content.rs`, an AppKit window on macOS, a Win32 popup on Windows                                                                  |
| `browser/`           | Browse mode: a native folder tree + thumbnail grid that swaps with the viewer + `browser::State`. AppKit on macOS, Win32 in `browser/windows/`, absent on Linux |
| `color/`             | ICC transform + display-profile detection (macOS, Windows) + Color settings panel + `color::State`                                                              |
| `decoding/`          | Image format decoders (JPEG via zune-jpeg; camera RAW via rawler; PNG/GIF/WebP/BMP/TIFF via `image`)                                                            |
| `diagnostics.rs`     | Performance observability: `NavigationRecord` + `build_text`                                                                                                    |
| `exif_overlay/`      | EXIF info overlay (`Settings::exif_visible` toggle, View → Exif info, bare `E` key)                                                                             |
| `file_associations/` | LaunchServices FFI + File associations settings panel                                                                                                           |
| `histogram/`         | 256-bin RGB histogram overlay (toggle via View → Histogram or `H` key) + `histogram::State`                                                                     |
| `navigation/`        | Directory scan + background preloader + LRU cache + `navigation::State`                                                                                         |
| `onboarding/`        | Onboarding window (macOS launch without a file) + defaults-sentence generator + checkmark renderer                                                              |
| `open_dialog.rs`     | The native "Open an image" picker behind File → Open, run off the event-loop thread through `rfd`                                                               |
| `qa/`                | Embedded HTTP + MCP JSON-RPC server                                                                                                                             |
| `settings/`          | JSON persistence + the AppKit settings window (shell + General panel) + the Win32 one (`settings/windows/`)                                                     |
| `slideshow/`         | Timer-driven auto-advance (`S`) + crossfade + Slideshow settings panel + `slideshow::State`                                                                     |
| `updater.rs`         | Update check: macOS installs the DMG, Windows opens the download                                                                                                |
| `window.rs`          | Main viewer window: create, fullscreen, auto-fit, title-bar vibrancy                                                                                            |
| `zoom/`              | `ViewState` + zoom/pan math + Zoom settings panel + `zoom::State`                                                                                               |

### Top-level principles

- **`winit` 0.30 `ApplicationHandler`.** The `App` struct implements the trait. Window and wgpu surface are created in
  `resumed()`, not at startup.
- **Render on demand.** `App.needs_redraw` gates frames.
- **`std::thread` + rayon for preloading.** No `tokio`.
- **Command architecture.** Every user action becomes an `AppCommand`. `App::execute_command` (`app/executor.rs`) is the
  single dispatcher.
- **Per-feature state.** `zoom::State`, `color::State`, `navigation::State` own feature-specific fields. `App` keeps
  only cross-cutting handles and runtime input.
- **Shared-state boundary.** `SharedAppState` (in `app/shared_state.rs`) is the snapshot the QA thread reads.

## Website (`apps/website/`)

Astro + Tailwind v4. Marketing site for getprvw.com.

| Path              | Purpose                         |
| ----------------- | ------------------------------- |
| `src/pages/`      | Astro pages (landing page)      |
| `src/layouts/`    | Base layout with OG tags, theme |
| `src/components/` | Reusable Astro components       |
| `src/styles/`     | Global CSS, color palette       |
| `public/`         | Static assets (fonts, favicon)  |

## Repo tasks (`xtask/`)

A dependency-free crate that reads the app's registries without building the app. `cargo xtask parity` renders
[`docs/parity.md`](parity.md), the generated table of what each platform's UI owes. See
[`xtask/CLAUDE.md`](../xtask/CLAUDE.md) and [`apps/desktop/src/parity/CLAUDE.md`](../apps/desktop/src/parity/CLAUDE.md).

## Scripts (`scripts/`)

| Path                | Purpose                                                      |
| ------------------- | ------------------------------------------------------------ |
| `check/`            | Go-based parallel check runner (same architecture as Cmdr's) |
| `check.sh`          | Shell wrapper for the check runner                           |
| `build-and-sign.sh` | Build, codesign, and bundle the macOS app                    |
