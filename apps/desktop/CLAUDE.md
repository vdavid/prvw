# Desktop app

The Prvw desktop app: a GPU-accelerated image viewer using `winit` + `wgpu` + `muda`.

## Architecture

The app struct implements `winit::application::ApplicationHandler`. The event loop drives everything.

| Module            | Responsibility                                              |
| ----------------- | ----------------------------------------------------------- |
| `main.rs`         | CLI parsing, event loop, `ApplicationHandler` impl          |
| `window.rs`       | Window creation, fullscreen toggle, title formatting        |
| `renderer.rs`     | wgpu surface, pipeline, texture upload, rendering           |
| `image_loader.rs` | Decode image files to RGBA8 via the `image` crate           |
| `view.rs`         | Zoom/pan math, transform uniform for GPU                    |
| `menu.rs`         | Native macOS menu bar via `muda`, shortcut wiring           |
| `directory.rs`    | Scan parent dir for images, sort, track position            |
| `preloader.rs`    | Background thread decoding, LRU cache (512 MB budget)       |
| `qa_server.rs`    | Embedded HTTP server for QA/E2E testing (state, commands, screenshots) |
| `shader.wgsl`     | WGSL vertex/fragment shader for textured quad with 2D transform |

## Key patterns

- **Surface lifecycle**: The wgpu surface and window are created in `resumed()`, not at startup. This is required by
  winit 0.30 on macOS. Creating them earlier crashes.
- **Render on demand**: The renderer only redraws when `needs_redraw` is true (set by zoom, pan, resize, or navigate).
  No continuous render loop. CPU/GPU usage is near zero when idle.
- **Preloader**: A `std::thread` (not tokio) decodes adjacent images via `std::sync::mpsc` channels. The `ImageCache`
  uses LRU eviction with a 512 MB memory budget.
- **Error display**: Errors go to the window title bar, not as text overlay. Text rendering in pure wgpu needs glyphon,
  which is overkill for v1.
- **Transform**: Zoom and pan are a 2D affine transform applied to the quad's vertices in the vertex shader. No image
  re-decode needed.

- **QA server**: An embedded HTTP server (raw `TcpListener`, no external crate) on a background thread. Agents and E2E
  tests use it to query state, send commands, and capture screenshots. Port controlled by `PRVW_QA_PORT` env var
  (default 19447, set to 0 to disable). Commands flow through `EventLoopProxy<AppCommand>` user events. Screenshots
  use an offscreen wgpu render target + buffer readback + PNG encoding.

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

## Dependencies

| Crate       | Version | Purpose                                  |
| ----------- | ------- | ---------------------------------------- |
| winit       | 0.30.13 | Windowing and event handling             |
| wgpu        | 29.0.1  | GPU rendering (Metal on macOS)           |
| pollster    | 0.4.0   | Block on wgpu async calls                |
| muda        | 0.17.2  | Native macOS menu bar                    |
| image       | 0.25.10 | Image decoding (JPEG, PNG, GIF, WebP, BMP, TIFF) |
| clap        | 4.6.0   | CLI argument parsing                     |
| log         | 0.4.29  | Logging facade                           |
| env_logger  | 0.11.10 | Log output to stderr                     |
| bytemuck    | 1.25.0  | Safe transmute for GPU uniform data      |
