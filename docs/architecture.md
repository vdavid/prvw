# Architecture

High-level map of Prvw's components. Each directory has (or will have) detailed docs in colocated `CLAUDE.md` files.

## Desktop app (`apps/desktop/`)

Pure Rust. No UI framework, no webview.

| Module            | Purpose                                                                                  |
| ----------------- | ---------------------------------------------------------------------------------------- |
| `main.rs`         | Entry point: CLI arg parsing, logging init, event loop creation                          |
| `window.rs`       | Window management via `winit` (`ApplicationHandler` trait), fullscreen toggle, resize     |
| `renderer.rs`     | `wgpu` surface, shader, texture upload, zoom/pan transform via uniform buffer             |
| `image_loader.rs` | Image decoding (`image` crate) to RGBA8, GPU texture upload, format support              |
| `view.rs`         | Zoom level, pan offset, fit-to-window, cursor-centered zoom math                         |
| `menu.rs`         | Native macOS menus via `muda`, shortcut wiring                                           |
| `directory.rs`    | Scan parent directory for image files, sort, track current position                      |
| `preloader.rs`    | Background `std::thread` that keeps N adjacent images decoded in memory (LRU, bounded)   |
| `shader.wgsl`     | WGSL shader for rendering a textured quad with 2D transform                              |

Key architecture decisions:

- **`winit` 0.30 `ApplicationHandler` trait**: the app struct implements `ApplicationHandler`. The `wgpu` surface and
  window are created in `resumed()`, not at startup (required for macOS correctness).
- **Render-on-demand**: the renderer only redraws on window events (resize, zoom, pan, navigate). No continuous render
  loop. This keeps CPU/GPU usage near zero when idle.
- **`std::thread` + channels for preloading**: CPU-bound image decoding runs on `std::thread`, not `tokio`. Communication
  via `std::sync::mpsc::channel`. This avoids async runtime weight and event-loop integration issues with `winit`.

## Website (`apps/website/`)

Astro + Tailwind v4. Marketing site for getprvw.com.

| Path                  | Purpose                              |
| --------------------- | ------------------------------------ |
| `src/pages/`          | Astro pages (landing page)           |
| `src/layouts/`        | Base layout with OG tags, theme      |
| `src/components/`     | Reusable Astro components            |
| `src/styles/`         | Global CSS, color palette            |
| `public/`             | Static assets (fonts, favicon)       |

## Scripts (`scripts/`)

| Path              | Purpose                                                        |
| ----------------- | -------------------------------------------------------------- |
| `check/`          | Go-based parallel check runner (same architecture as Cmdr's)   |
| `check.sh`        | Shell wrapper for the check runner                             |
| `build-and-sign.sh` | Build, codesign, and bundle the macOS app                   |
