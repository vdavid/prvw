# Architecture

High-level map of Prvw's components. Each module has a colocated `CLAUDE.md` (or
`//!` module docs for single-file features); this page is the index.

## Desktop app (`apps/desktop/`)

Pure Rust (`winit` + `wgpu` + `muda`). Flat `src/` layout: infrastructure and features
sit as siblings. `App` holds per-feature state via `zoom::State`, `color::State`,
`navigation::State`.

### Source layout (`src/`)

**Infrastructure:**

| Path                          | Role                                                              |
| ----------------------------- | ----------------------------------------------------------------- |
| `main.rs`                     | Thin entry: CLI, logger, event-loop setup                         |
| `app.rs` + `app/`             | `App`, `ApplicationHandler`, command dispatcher, `SharedAppState` |
| `commands.rs`                 | `AppCommand` enum + global `EventLoopProxy`                       |
| `input.rs`                    | Maps keys / menu events / QA keys to `AppCommand`                 |
| `menu.rs`                     | Native macOS menu bar via `muda`                                  |
| `pixels.rs`                   | `Logical` / `Physical` newtypes for coordinate types              |
| `platform.rs` + `platform/`   | Cross-cutting platform glue (Apple Events, AppKit helpers)        |
| `render.rs` + `render/`       | wgpu infrastructure: renderer, text overlay, shaders              |

**Features:**

| Path                  | Owns                                                                                    |
| --------------------- | --------------------------------------------------------------------------------------- |
| `about.rs`            | About window                                                                            |
| `color/`              | ICC transform + display-profile detection (macOS) + Color settings panel + `color::State` |
| `decoding.rs`         | Image format decoders (JPEG via zune-jpeg; PNG/GIF/WebP/BMP/TIFF via `image`)           |
| `diagnostics.rs`      | Performance observability — `NavigationRecord` + `build_text`                           |
| `file_associations/`  | LaunchServices FFI + File associations settings panel                                   |
| `navigation/`         | Directory scan + background preloader + LRU cache + `navigation::State`                 |
| `onboarding.rs`       | Onboarding window (first launch without a file)                                         |
| `qa/`                 | Embedded HTTP + MCP JSON-RPC server                                                     |
| `settings/`           | JSON persistence + Settings window shell + General panel                                |
| `updater.rs`          | Auto-update check (GitHub releases)                                                     |
| `window.rs`           | Main viewer window: create, fullscreen, auto-fit, title-bar vibrancy                    |
| `zoom/`               | `ViewState` + zoom/pan math + Zoom settings panel + `zoom::State`                       |

### Top-level principles

- **`winit` 0.30 `ApplicationHandler`.** The `App` struct implements the trait.
  Window and wgpu surface are created in `resumed()`, not at startup.
- **Render on demand.** `App.needs_redraw` gates frames.
- **`std::thread` + rayon for preloading.** No `tokio`.
- **Command architecture.** Every user action becomes an `AppCommand`.
  `App::execute_command` (`app/executor.rs`) is the single dispatcher.
- **Per-feature state.** `zoom::State`, `color::State`, `navigation::State` own
  feature-specific fields. `App` keeps only cross-cutting handles and runtime input.
- **Shared-state boundary.** `SharedAppState` (in `app/shared_state.rs`) is the
  snapshot the QA thread reads.

## Website (`apps/website/`)

Astro + Tailwind v4. Marketing site for getprvw.com.

| Path              | Purpose                              |
| ----------------- | ------------------------------------ |
| `src/pages/`      | Astro pages (landing page)           |
| `src/layouts/`    | Base layout with OG tags, theme      |
| `src/components/` | Reusable Astro components            |
| `src/styles/`     | Global CSS, color palette            |
| `public/`         | Static assets (fonts, favicon)       |

## Scripts (`scripts/`)

| Path                | Purpose                                                      |
| ------------------- | ------------------------------------------------------------ |
| `check/`            | Go-based parallel check runner (same architecture as Cmdr's) |
| `check.sh`          | Shell wrapper for the check runner                           |
| `build-and-sign.sh` | Build, codesign, and bundle the macOS app                    |
