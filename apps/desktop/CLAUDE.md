# Desktop app

The Prvw desktop app: a GPU-accelerated image viewer using `winit` + `wgpu` + `muda`.

## Source layout

The source tree is organized into module directories; each has its own `CLAUDE.md`
with the detailed patterns, gotchas, and decision history for that area.

| Path                   | Role                                                                            |
| ---------------------- | ------------------------------------------------------------------------------- |
| `src/main.rs`          | Thin entry: CLI, logger, event-loop creation, hands off to `App`                |
| `src/app/`             | `App` struct + `ApplicationHandler` impl + `execute_command` dispatcher ([CLAUDE.md](src/app/CLAUDE.md)) |
| `src/commands.rs`      | `AppCommand` enum and the global `EventLoopProxy` shim                          |
| `src/imaging/`         | Load → decode → color-transform → cache → navigate ([CLAUDE.md](src/imaging/CLAUDE.md)) |
| `src/render/`          | `wgpu` renderer + zoom/pan math + text overlay + shaders ([CLAUDE.md](src/render/CLAUDE.md)) |
| `src/platform/macos/`  | macOS integrations: display ICC, Apple Events, file associations, auto-update ([CLAUDE.md](src/platform/macos/CLAUDE.md)) |
| `src/platform/macos/native_ui/` | AppKit secondary windows: About, Onboarding, Settings ([CLAUDE.md](src/platform/macos/native_ui/CLAUDE.md)) |
| `src/qa/`              | Embedded HTTP/MCP server for automated QA ([CLAUDE.md](src/qa/CLAUDE.md))       |
| `src/window.rs`        | Main viewer window: creation, fullscreen, auto-fit, title-bar vibrancy           |
| `src/menu.rs`          | Native macOS menu bar via `muda`                                                |
| `src/input.rs`         | Maps keys/menus/QA events to `AppCommand`s                                      |
| `src/pixels.rs`        | `Logical`/`Physical` newtypes for coordinate types                              |
| `src/settings.rs`      | JSON persistence (overridable via `PRVW_DATA_DIR`)                              |

Also: `apps/desktop/tests/` — integration tests that drive the QA server.

## Top-level principles

- **Surface lifecycle.** The `wgpu` surface and window must be created in `resumed()`,
  not at startup. Required by winit 0.30 on macOS.
- **Render on demand.** `App.needs_redraw` gates frames. No continuous render loop —
  CPU/GPU near zero when idle.
- **Command architecture.** All user actions become `AppCommand` variants.
  `App::execute_command` (in `app/executor.rs`) is the single dispatcher.
- **No `tokio`.** CPU-bound work runs on `std::thread` + rayon. `mpsc` channels move
  data across thread boundaries. Event-loop integration with winit is simpler without
  a runtime.
- **Zoom is absolute.** `zoom=1.0` means 1 image pixel = 1 screen pixel. `fit_zoom()`
  is the zoom that fills the window. `min_zoom` is the floor. On image load,
  `apply_initial_zoom()` picks both based on the auto-fit and enlarge settings. See
  `app/CLAUDE.md` for the full matrix.
- **Coordinate conventions.** `Logical<T>` (f64, UI layout) vs `Physical<T>` (u32, GPU
  pixels). `pixels.rs` defines both. `scale_factor` converts between them. Retina =
  2.0. When in doubt, check the function signature.

## Cross-cutting gotchas

These live at the top because they span multiple modules. Module-specific gotchas are
in the colocated `CLAUDE.md` files.

- **Never run AppKit modals inside winit's event loop.** Nested run loops segfault on
  autorelease pool cleanup. Run native modals BEFORE `EventLoop::new()` (like the
  pre-launch onboarding in `main()`), or defer via `EventLoopProxy`.
- **`Retained<>` lifetime with AppKit.** Every `Retained<NSTextField/NSButton/...>`
  must stay alive for the window's lifetime — store them in a `Vec` that outlives the
  window. Dropping early = segfault. No compile-time check.
- **Finder file opens need ObjC method injection.** winit 0.30 registers its own
  `NSApplicationDelegate` and panics if replaced, and doesn't implement
  `application:openURLs:`. `platform/macos/open_handler::register()` uses
  `class_addMethod` after `EventLoop::new()` but before `run_app()`.
- **`zune-jpeg` in debug builds.** Its SIMD path is unusable without optimizations.
  `Cargo.toml` sets `[profile.dev.package.zune-jpeg] opt-level = 3`.

## Running

- Dev: `cd apps/desktop && cargo run -- <image_path>`
- Release: `cd apps/desktop && cargo run --release -- <image_path>`
- With debug logging: `RUST_LOG=debug cargo run -- <image_path>`
- Target a specific module: `RUST_LOG=prvw::render::renderer=debug ...`

## Tests

- All Rust checks (fmt, clippy, unit + integration tests): `./scripts/check.sh --rust`
- Specific test: `cd apps/desktop && cargo test <test_name>`
- Integration tests drive the QA server; see `tests/integration.rs` and `qa/CLAUDE.md`.

## Dependencies

| Crate                 | Version | Purpose                                                                     |
| --------------------- | ------- | --------------------------------------------------------------------------- |
| winit                 | 0.30.13 | Windowing and event handling                                                |
| wgpu                  | 29.0.1  | GPU rendering (Metal on macOS)                                              |
| pollster              | 0.4.0   | Block on wgpu async calls                                                   |
| muda                  | 0.17.2  | Native macOS menu bar                                                       |
| image                 | 0.25.10 | Image decoding (PNG, GIF, WebP, BMP, TIFF) and PNG encoding for screenshots |
| zune-jpeg             | 0.5.15  | Fast JPEG decoding with SIMD (replaces `image` for JPEG)                    |
| zune-core             | 0.5.1   | Decoder options for zune-jpeg                                               |
| moxcms                | 0.8.1   | ICC color management, pure Rust with NEON SIMD                              |
| rayon                 | 1.11.0  | Thread pool for parallel preloading                                         |
| clap                  | 4.6.0   | CLI argument parsing                                                        |
| log                   | 0.4.29  | Logging facade                                                              |
| env_logger            | 0.11.10 | Log output to stderr                                                        |
| bytemuck              | 1.25.0  | Safe transmute for GPU uniform data                                         |
| objc2-core-foundation | 0.3     | CFString for CoreServices FFI                                               |
| objc2-core-services   | 0.3     | File association APIs (LSSetDefaultRoleHandler, etc.)                       |
| glyphon               | current | Text rendering on top of wgpu                                               |
