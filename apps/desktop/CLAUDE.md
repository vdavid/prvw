# Desktop app

The Prvw desktop app: a GPU-accelerated image viewer using `winit` + `wgpu` + `muda`.

## Architecture

The app struct implements `winit::application::ApplicationHandler`. The event loop drives everything.

| Module            | Responsibility                                              |
| ----------------- | ----------------------------------------------------------- |
| `main.rs`         | CLI parsing, event loop, `ApplicationHandler` impl, command executor |
| `input.rs`        | Maps keyboard, mouse, menu, and QA key events to `AppCommand`s |
| `window.rs`       | Window creation, fullscreen toggle, auto-fit resize, title formatting |
| `renderer.rs`     | wgpu surface, pipeline, texture upload, rendering           |
| `image_loader.rs` | Decode image files to RGBA8 (zune-jpeg for JPEG, `image` crate for others) |
| `view.rs`         | Zoom/pan math, transform uniform for GPU                    |
| `menu.rs`         | Native macOS menu bar via `muda`, shortcut wiring           |
| `directory.rs`    | Scan parent dir for images, sort, track position            |
| `preloader.rs`    | Parallel background decoding (rayon pool), LRU cache (512 MB budget) |
| `native_ui.rs`    | AppKit secondary windows (About, Onboarding, Settings) via objc2 |
| `onboarding.rs`   | File association queries and default viewer registration helpers |
| `settings.rs`     | Settings persistence (JSON file in app data dir, overridable via `PRVW_DATA_DIR` env var) |
| `qa_server.rs`    | `AppCommand` enum, embedded HTTP/MCP server for QA/E2E, global event loop proxy |
| `shader.wgsl`     | WGSL vertex/fragment shader for textured quad with 2D transform |

## Key patterns

- **Surface lifecycle**: The wgpu surface and window are created in `resumed()`, not at startup. This is required by
  winit 0.30 on macOS. Creating them earlier crashes.
- **Render on demand**: The renderer only redraws when `needs_redraw` is true (set by zoom, pan, resize, or navigate).
  No continuous render loop. CPU/GPU usage is near zero when idle.
- **Preloader**: A rayon thread pool (min(4, cores-1) threads) decodes adjacent images in parallel, sending results back
  via `std::sync::mpsc`. An in-flight `HashSet` prevents duplicate work. The `ImageCache` uses LRU eviction with a 512
  MB memory budget.
- **Error display**: Errors go to the window title bar, not as text overlay. Text rendering in pure wgpu needs glyphon,
  which is overkill for v1.
- **Transform**: Zoom and pan are a 2D affine transform applied to the quad's vertices in the vertex shader. No image
  re-decode needed.

- **Zoom model**: Zoom is absolute: `zoom=1.0` means 1 image pixel = 1 screen pixel. `fit_zoom()` computes the zoom
  that makes the image exactly fit the window (< 1.0 for large images, > 1.0 for small ones). The zoom floor
  (`min_zoom`) prevents zooming out past fit. On image load, `apply_initial_zoom()` sets the floor and starting zoom:
  - **Auto-fit ON** (enlarge ignored): window resizes to image. `min_zoom` = zoom at which window hits 200px minimum.
    On zoom in/out, the window resizes to match (`auto_fit_after_zoom`): desired size = image * zoom, capped at 90%
    screen, floored at 200px. The cursor pivot stays at the same screen pixel. When the window hits the screen cap,
    the leftover zoom is handled by panning within the fixed-size window.
  - **Auto-fit OFF, Enlarge ON**: `min_zoom=fit_zoom`, initial zoom=`fit_zoom` (small images enlarged).
  - **Auto-fit OFF, Enlarge OFF, large image** (`fit_zoom < 1.0`): `min_zoom=fit_zoom`, initial zoom=`fit_zoom`.
  - **Auto-fit OFF, Enlarge OFF, small image** (`fit_zoom > 1.0`): `min_zoom=1.0`, initial zoom=1.0 (native pixels).
  "Fit to window" (0 key) always sets zoom=`fit_zoom`. "Actual size" (1 key) always sets zoom=1.0. The zoom % in the
  titlebar is the actual pixel scale (100% = native size). Background is always black.

- **Command architecture**: All user actions are expressed as `AppCommand` variants (defined in `qa_server.rs`).
  `input.rs` maps keyboard, menu, and QA key events to commands. `App::execute_command()` in `main.rs` is the single
  place where each command's effect is implemented. Scroll zoom, mouse drag, and cursor tracking stay inline in
  `window_event` since they're continuous input, not discrete commands.

- **QA server**: An embedded HTTP server (raw `TcpListener`, no external crate) on a background thread. Agents and E2E
  tests use it to query state, send commands, and capture screenshots. Port controlled by `PRVW_QA_PORT` env var
  (default 19447, set to 0 to disable). Commands flow through `EventLoopProxy<AppCommand>` user events. Screenshots
  use an offscreen wgpu render target + buffer readback + PNG encoding.

- **Native secondary windows** (`native_ui.rs`): About, Onboarding, and Settings windows are built with AppKit via
  objc2. All use `NSStackView` for layout, `NSVisualEffectView` for frosted glass, and transparent titlebars.
  - **Onboarding** runs as a modal (`runModalForWindow`) BEFORE `EventLoop::new()`. Uses a state/render separation
    pattern: `OnboardingState` (pure data, no UI refs) computes current state from system queries, and `OnboardingUI`
    (widget pointers) has a single `render()` method that applies state to all widgets. An `NSTimer` polls every second,
    and the delegate's button handler both use `OnboardingState::current()` + `ui.render()`. After the modal exits, the
    timer is invalidated and views are dropped.
  - **About and Settings** are non-modal: `makeKeyAndOrderFront` + `mem::forget` the retained views. A deduplication
    guard (`is_window_already_open`) prevents stacking. FIXME: views leak on close/reopen (see code comments).
  - **Settings** uses a `define_class!` delegate (`SettingsDelegate`) for the NSSwitch toggle actions. Toggles
    apply immediately (no confirm step) — the button is "Close", not "OK". Changes route through
    `AppCommand::SetAutoFitWindow` / disk write so the menu checkmarks and app state stay in sync.

- **Global event loop proxy** (`qa_server.rs`): A `OnceLock<EventLoopProxy<AppCommand>>` is set once in `resumed()`.
  This lets non-event-loop code (like the native Settings delegate) send commands into the main loop. Used by
  `send_command()` — the same mechanism the QA server uses, just without needing a reference to the proxy.

- **File associations** (`onboarding.rs`): Uses `LSCopyDefaultRoleHandlerForContentType` and
  `LSSetDefaultRoleHandlerForContentType` via objc2-core-services FFI. No Swift scripts — direct C calls, near-instant.

## Gotchas

- **wgpu 29 API changes**: `Instance::new()` takes a value (not reference). `get_current_texture()` returns
  `CurrentSurfaceTexture` enum (not `Result`). `PipelineLayoutDescriptor` uses `immediate_size` instead of
  `push_constant_ranges`. `RenderPassColorAttachment` requires `depth_slice`. `mipmap_filter` uses `MipmapFilterMode`.
- **winit 0.30 `ApplicationHandler`**: No closure-based `run`. The app struct implements the trait. State that depends on
  the window (renderer, surface) must be `Option` and initialized in `resumed()`.
- **muda menu**: `init_for_nsapp()` must be called after building the menu. Menu events are polled via
  `MenuEvent::receiver().try_recv()`, not callbacks.
- **bytemuck derives**: Use `bytemuck::Pod` and `bytemuck::Zeroable` (from the `derive` feature), not
  `bytemuck_derive::Pod` directly.
- **zune-jpeg in debug builds**: zune-jpeg's SIMD is painfully slow without optimizations. `Cargo.toml` sets
  `[profile.dev.package.zune-jpeg] opt-level = 3` to fix this.
- **objc2 `Retained<>` lifetime with AppKit modals**: when creating AppKit views (NSTextField, NSButton, etc.) via
  objc2 and adding them to a parent view with `addSubview`, the Rust `Retained<>` wrapper must stay alive for the
  entire duration of the modal session. If it drops (goes out of scope), AppKit's autorelease pool cleanup will
  segfault (use-after-free). Fix: collect all views in a `Vec<Retained<...>>` that lives alongside the modal loop.
  This applies to `native_ui.rs` and any future native macOS dialogs. There is no compile-time check for this.
- **Never run AppKit modals from inside winit's event loop.** Running `NSApplication::runModalForWindow` inside
  winit's `resumed()` or `window_event()` creates a nested run loop inside winit's autorelease pool. When the modal
  ends and an Apple Event arrives, the pool drains objects from the wrong scope, causing segfault. Fix: run native
  modals BEFORE `EventLoop::new()` (like the onboarding dialog in `main()`), or use `EventLoopProxy` to defer the
  modal to after the event loop exits.
- **`define_class!` methods get an implicit `_cmd: Sel` parameter.** Plain helper methods defined inside
  `define_class!` are treated as ObjC methods and receive an implicit selector argument. To define a plain Rust
  helper, put it in a separate `impl` block outside the macro, or use a free function.
- **`request_inner_size` is async on macOS.** After calling `window.request_inner_size()`, `window.inner_size()`
  still returns the OLD size. The `Resized` event arrives later. To avoid a frame of wrong proportions,
  `resize_to_fit_image` computes and returns the physical size so callers can pass it directly to `renderer.resize()`.
- **`msg_send!` return types must match the ObjC method signature exactly.** `setActivationPolicy:` returns `BOOL`,
  not `void`. Writing `let _: () = msg_send![...]` for a method that returns `BOOL` panics at runtime with
  "expected return to have type code 'B', but found 'v'". Always check Apple's docs for the return type.

## Dependencies

| Crate       | Version | Purpose                                  |
| ----------- | ------- | ---------------------------------------- |
| winit       | 0.30.13 | Windowing and event handling             |
| wgpu        | 29.0.1  | GPU rendering (Metal on macOS)           |
| pollster    | 0.4.0   | Block on wgpu async calls                |
| muda        | 0.17.2  | Native macOS menu bar                    |
| image       | 0.25.10 | Image decoding (PNG, GIF, WebP, BMP, TIFF) and PNG encoding for screenshots |
| zune-jpeg   | 0.5.15  | Fast JPEG decoding with SIMD (replaces `image` for JPEG) |
| zune-core   | 0.5.1   | Decoder options for zune-jpeg                    |
| rayon       | 1.11.0  | Thread pool for parallel preloading               |
| clap        | 4.6.0   | CLI argument parsing                     |
| log         | 0.4.29  | Logging facade                           |
| env_logger  | 0.11.10 | Log output to stderr                     |
| bytemuck    | 1.25.0  | Safe transmute for GPU uniform data      |
| objc2-core-foundation | 0.3 | CFString for CoreServices FFI       |
| objc2-core-services   | 0.3 | File association APIs (LSSetDefaultRoleHandler, etc.) |
