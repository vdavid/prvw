# Architecture

High-level map of Prvw's components. Each module has a colocated `CLAUDE.md` with the
detailed docs; this page is the index.

## Desktop app (`apps/desktop/`)

Pure Rust (`winit` + `wgpu` + `muda`). No UI framework, no webview.

### Source layout (`src/`)

| Path                   | Role                                                                            | Colocated docs |
| ---------------------- | ------------------------------------------------------------------------------- | -------------- |
| `main.rs`              | Thin entry: CLI, logger, event-loop creation, hands off to `App`                | —              |
| `app.rs` + `app/`      | `App` struct, `ApplicationHandler` impl, and `execute_command` (in `executor.rs`) | `app/CLAUDE.md` |
| `commands.rs`          | `AppCommand` enum + global `EventLoopProxy`                                     | —              |
| `imaging.rs` + `imaging/` | Load → decode → color-transform → cache → navigate (`loader`, `color`, `preloader`, `directory`) | `imaging/CLAUDE.md` |
| `render.rs` + `render/`   | `wgpu` renderer, zoom/pan math, text overlay, shaders                         | `render/CLAUDE.md` |
| `platform.rs` + `platform/macos/` | macOS integrations (display ICC, Apple Events, file associations, AppKit windows, auto-update) | `platform/macos/CLAUDE.md`, `platform/macos/native_ui/CLAUDE.md` |
| `qa.rs` + `qa/`        | Embedded HTTP/MCP server for automated QA                                       | `qa/CLAUDE.md` |
| `window.rs`            | Main viewer window: creation, fullscreen, auto-fit resize, title-bar vibrancy   | —              |
| `menu.rs`              | Native macOS menu bar via `muda`                                                | —              |
| `input.rs`             | Maps key/menu/QA events to `AppCommand`s                                        | —              |
| `pixels.rs`            | `Logical`/`Physical` newtypes for coordinate types                              | —              |
| `settings.rs`          | JSON persistence (overridable via `PRVW_DATA_DIR`)                              | —              |

### Top-level principles

- **`winit` 0.30 `ApplicationHandler`.** The `App` struct implements the trait. Window
  and `wgpu` surface are created in `resumed()`, not at startup (required for macOS).
- **Render on demand.** `App.needs_redraw` gates frames. No continuous render loop.
- **`std::thread` + channels for preloading.** CPU-bound decoding runs on
  `std::thread` (via rayon). `tokio` is not a fit — too much weight and event-loop
  integration friction with `winit`.
- **Command architecture.** Every user action becomes an `AppCommand`. `App::execute_command`
  (`app/executor.rs`) is the one place each variant's effect is implemented. QA, MCP,
  menu clicks, keys, and AppKit toggle delegates all funnel through this.

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
