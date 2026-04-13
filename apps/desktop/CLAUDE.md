# Desktop app

The Prvw desktop app: a Tauri 2 image viewer with a Rust backend and Svelte 5 + SvelteKit frontend rendered in a
webview.

## Architecture

The Rust backend (`src-tauri/`) handles image loading, decoding, preloading, directory scanning, menus, and the QA
server. The frontend (`src/`) is a Svelte 5 + SvelteKit SPA (adapter-static, SSR disabled) that displays images via
Tauri's asset protocol and handles zoom/pan/keyboard input in the browser. Vite serves the dev frontend on port 14200.

### Rust backend (`src-tauri/src/`)

| Module                  | Responsibility                                                             |
| ----------------------- | -------------------------------------------------------------------------- |
| `main.rs`               | CLI parsing, Tauri app setup, command registration, Apple Event handler    |
| `image_loader.rs`       | Decode image files to RGBA8 (zune-jpeg for JPEG, `image` crate for others) |
| `view.rs`               | Zoom/pan math, fit-to-window calculations                                  |
| `menu.rs`               | Native macOS menu bar via Tauri's menu API                                 |
| `directory.rs`          | Scan parent dir for images, sort, track position                           |
| `preloader.rs`          | Parallel background decoding (rayon pool), LRU cache (512 MB budget)       |
| `mcp/`                  | MCP server for AI agent integration (tools, resources, JSON-RPC)           |
| `onboarding.rs`         | macOS onboarding helpers (file associations, app bundle detection)         |
| `settings.rs`           | Settings data types (updates_enabled, etc.)                                |
| `updater.rs`            | Update checking logic                                                      |
| `macos_open_handler.rs` | Apple Event handler for `open` events (double-click file in Finder)        |

### Frontend (`src/`)

SvelteKit SPA with adapter-static. SSR disabled in `+layout.ts`.

| File/Dir                | Responsibility                                              |
| ----------------------- | ----------------------------------------------------------- |
| `app.html`              | SvelteKit shell template                                    |
| `app.css`               | Design tokens (colors, spacing, fonts, radii) and CSS reset |
| `routes/+layout.svelte` | Root layout, imports `app.css`                              |
| `routes/+layout.ts`     | Disables SSR (`export const ssr = false`)                   |
| `routes/+page.svelte`   | Main page (placeholder for now)                             |

### Tooling (`scripts/`, config files)

| File                       | Responsibility                                                            |
| -------------------------- | ------------------------------------------------------------------------- |
| `scripts/tauri-wrapper.js` | Wraps `tauri` CLI: injects dev config, defaults to universal macOS binary |
| `src-tauri/tauri.dev.json` | Dev-only Tauri overrides (`withGlobalTauri: true`)                        |
| `vite.config.js`           | Vite + SvelteKit, port 1420                                               |
| `svelte.config.js`         | adapter-static, vitePreprocess                                            |
| `eslint.config.js`         | Flat ESLint config: TypeScript + Svelte, complexity max 15                |
| `.stylelintrc.mjs`         | Enforces design tokens, no raw hex/px in components                       |

## Key patterns

- **Tauri commands**: The frontend calls Rust functions via `invoke()`. Commands are registered in `main.rs` with
  `.invoke_handler(tauri::generate_handler![...])`.
- **Asset protocol**: Images are served to the webview via `asset://localhost/{path}`. The scope in `tauri.conf.json`
  allows all paths. This avoids base64-encoding images over IPC.
- **CSS zoom/pan**: The frontend handles zoom and pan with CSS transforms on the `<img>` element, keeping the rendering
  in the webview's compositor rather than going through Rust.
- **Browser preloading**: Adjacent images are preloaded by creating `Image()` objects in JS that fetch via the asset
  protocol, warming the webview's cache.
- **Preloader**: A rayon thread pool (min(4, cores-1) threads) decodes adjacent images in parallel, sending results back
  via `std::sync::mpsc`. An in-flight `HashSet` prevents duplicate work. The `ImageCache` uses LRU eviction with a 512
  MB memory budget.
- **Onboarding**: When launched with no files from a .app bundle, the frontend shows a welcome screen instead of the
  image viewer.
- **MCP server**: An Axum-based HTTP server on `127.0.0.1:19447` implementing JSON-RPC 2.0 (Model Context Protocol).
  Tools emit Tauri events directly; resources read from `SharedAppState`. See `src-tauri/src/mcp/CLAUDE.md`.

## Decisions

- **`withGlobalTauri` in dev overlay only**: Moved from `tauri.conf.json` to `tauri.dev.json` so production builds don't
  expose the Tauri API on `window.__TAURI__`. The `tauri-wrapper.js` script injects the dev config automatically.

## Gotchas

- **Never use `cargo run` or `cargo build` to run the app.** The Tauri binary without the embedded frontend is a white
  screen. Use `pnpm tauri dev` (which runs Vite + Cargo together) or `pnpm tauri build` for release. `cargo check`,
  `cargo test`, and `cargo clippy` are fine — they don't produce runnable binaries.
- **zune-jpeg in debug builds**: zune-jpeg's SIMD is painfully slow without optimizations. The workspace `Cargo.toml`
  sets `[profile.dev.package.zune-jpeg] opt-level = 3` to fix this.
- **Tauri asset protocol scope**: The `assetProtocol.scope.allow` in `tauri.conf.json` must include the paths you want
  to serve. Currently set to `["**"]` (all paths).
- **image crate version**: Pinned to 0.25.6 for compatibility. Check before upgrading.

## Dependencies

| Crate              | Version | Purpose                                             |
| ------------------ | ------- | --------------------------------------------------- |
| tauri              | 2.10.3  | App framework (webview, IPC, bundling)              |
| image              | 0.25.6  | Image decoding and PNG encoding                     |
| zune-jpeg          | 0.5.15  | Fast JPEG decoding with SIMD                        |
| zune-core          | 0.5.1   | Decoder options for zune-jpeg                       |
| rayon              | 1.11.0  | Thread pool for parallel preloading                 |
| clap               | 4.6.0   | CLI argument parsing                                |
| serde              | 1.0.228 | Serialization for Tauri commands                    |
| nom-exif           | 2.7.0   | EXIF metadata parsing                               |
| log                | 0.4.29  | Logging facade                                      |
| tauri-plugin-log   | 2.8.0   | Unified logging (Rust + webview console forwarding) |
| tauri-plugin-store | 2.4.2   | Key-value settings persistence                      |
| axum               | 0.8.8   | MCP server HTTP transport                           |
| tokio              | 1.51.1  | Async runtime for axum                              |
