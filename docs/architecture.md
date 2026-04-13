# Architecture

High-level map of Prvw's components. Each directory has (or will have) detailed docs in colocated `CLAUDE.md` files.

## Desktop app (`apps/desktop/`)

Tauri 2 app: Rust backend + HTML/CSS/JS frontend in a webview.

| Path                       | Purpose                                                                   |
| -------------------------- | ------------------------------------------------------------------------- |
| `src-tauri/src/main.rs`    | Entry point: CLI parsing, Tauri app setup, command registration           |
| `src-tauri/src/image_loader.rs` | Image decoding to RGBA8 (zune-jpeg for JPEG, `image` crate for others) |
| `src-tauri/src/view.rs`    | Zoom/pan math, fit-to-window calculations                                |
| `src-tauri/src/menu.rs`    | Native macOS menu bar via Tauri's menu API                               |
| `src-tauri/src/directory.rs` | Scan parent directory for image files, sort, track current position     |
| `src-tauri/src/preloader.rs` | Background rayon pool: parallel image decoding, LRU cache (512 MB)     |
| `src-tauri/src/qa_server.rs` | Embedded HTTP server for QA/E2E testing                                |
| `src/index.html`           | Frontend: page structure                                                  |
| `src/style.css`            | Frontend: styling, CSS zoom/pan transforms                                |
| `src/app.js`               | Frontend: Tauri IPC, keyboard/mouse handling, navigation                  |

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
