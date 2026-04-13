# Architecture

High-level map of Prvw's components. Each directory has (or will have) detailed docs in colocated `CLAUDE.md` files.

## Desktop app (`apps/desktop/`)

Tauri 2 app: Rust backend + Svelte 5/SvelteKit frontend in a webview.

| Path                              | Purpose                                                               |
| --------------------------------- | --------------------------------------------------------------------- |
| `src-tauri/src/main.rs`           | Entry point: CLI parsing, Tauri app setup, command registration       |
| `src-tauri/src/image_loader.rs`   | Image decoding to RGBA8 (zune-jpeg for JPEG, `image` crate for rest) |
| `src-tauri/src/menu.rs`           | Native macOS menu bar via Tauri's menu API                            |
| `src-tauri/src/directory.rs`      | Scan parent directory for image files, sort, track position           |
| `src-tauri/src/preloader.rs`      | Background rayon pool: parallel image decoding, LRU cache (512 MB)   |
| `src-tauri/src/mcp/`              | MCP server for AI agent control (JSON-RPC 2.0, Axum HTTP)            |
| `src-tauri/src/settings.rs`       | Settings types, disk persistence via tauri-plugin-store               |
| `src-tauri/src/fe_log.rs`         | Frontend log bridge (receives batched logs from webview)              |
| `src/lib/components/ImageViewer.svelte` | Image display, zoom/pan, navigation (imperative DOM)            |
| `src/lib/components/Header.svelte`| Overlay: filename, position, zoom %                                   |
| `src/lib/tauri.ts`                | Typed wrappers for Tauri IPC commands                                 |
| `src/lib/log-bridge.ts`           | Forwards console.log to Rust via batch_fe_logs                        |

Key architecture decisions:

- **Tauri 2 with asset protocol**: Images are served to the webview via `asset://localhost/` instead of base64 encoding
  over IPC. This keeps memory usage low and rendering fast.
- **CSS zoom/pan**: Zoom and pan are handled with CSS transforms on the `<img>` element, staying in the webview's
  compositor.
- **`std::thread` + channels for preloading**: CPU-bound image decoding runs on `std::thread` (via rayon), not `tokio`.
  Communication via `std::sync::mpsc::channel`. This avoids async runtime weight.

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
