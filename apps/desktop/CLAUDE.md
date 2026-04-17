# Desktop app

Feature-based, flat layout. Each directory under `src/` is either infrastructure
(used by every feature) or one user-visible feature. No `features/` wrapper.

## Source layout

```
src/
├── main.rs                  Thin entry: CLI, logger, event-loop setup
│
│   Infrastructure:
├── app.rs + app/            App struct, ApplicationHandler, command dispatcher, shared-state snapshot
├── commands.rs              AppCommand enum + global EventLoopProxy
├── input.rs                 Maps keys/menu/QA keys → AppCommand
├── menu.rs                  Native macOS menu bar (muda)
├── pixels.rs                Logical/Physical coordinate newtypes
├── platform.rs + platform/  Cross-cutting platform glue (Apple Events, AppKit helpers)
├── render.rs + render/      wgpu infrastructure (renderer, text, shaders)
│
│   Features:
├── about.rs                 About window
├── color/                   ICC transform + display profile (macOS) + Color settings panel + color::State
├── decoding.rs              Image format decoders
├── diagnostics.rs           Performance observability (cache/nav/RSS formatter)
├── file_associations/       LaunchServices FFI + File associations settings panel
├── navigation/              Directory scan + preloader + LRU cache + navigation::State
├── onboarding.rs            Onboarding window
├── qa/                      Embedded HTTP + MCP server
├── settings/                JSON persistence + Settings window shell + widgets + General panel
├── updater.rs               Auto-update
├── window.rs                Main viewer window: create, fullscreen, auto-fit, vibrancy
└── zoom/                    ViewState + zoom/pan math + Zoom settings panel + zoom::State
```

Single-file features (`about.rs`, `decoding.rs`, `diagnostics.rs`, `onboarding.rs`,
`updater.rs`, `window.rs`) use their `//!` module docs in place of a `CLAUDE.md`.
Directory-based features have a colocated `CLAUDE.md`.

## Per-feature state

`App` holds `zoom: zoom::State`, `color: color::State`, `navigation: navigation::State`.
Each feature's runtime state lives in its own module. App only keeps truly
cross-cutting state — handles (window, renderer, menu), launch flags (file_path,
waiting_for_file), runtime input (modifiers, drag_start, etc.), and the single
cross-feature toggle `title_bar`.

## Top-level principles

- **`winit` 0.30 `ApplicationHandler`.** App implements the trait. Window + wgpu
  surface created in `resumed()`, not startup (required on macOS).
- **Render on demand.** `App.needs_redraw` gates frames. No continuous render loop.
- **Command architecture.** Every user action becomes an `AppCommand` in
  `crate::commands`. `App::execute_command` (`app/executor.rs`) is the single
  dispatcher. Keys, menus, QA HTTP, MCP, AppKit delegates all funnel there.
- **No `tokio`.** CPU-bound decoding runs on `std::thread` via rayon. `mpsc` channels
  cross threads.
- **Shared-state boundary.** `SharedAppState` (in `app/shared_state.rs`) is the
  snapshot the QA thread reads. Main thread writes on every observable change;
  diagnostics text is rendered by `diagnostics::build_text`.

## Cross-cutting gotchas

See `platform/macos/CLAUDE.md` for the full list. Short version:

- **Never run AppKit modals inside winit's event loop** — segfault. Run them before
  `EventLoop::new()` or defer via `EventLoopProxy`.
- **`Retained<>` outlives the window.** Store every objc2 `Retained<...>` in a `Vec`
  that outlives the window. No compile-time check.
- **Finder file opens need ObjC method injection** into winit's delegate — see
  `platform/macos/open_handler.rs`.
- **`zune-jpeg` in debug builds.** SIMD unusably slow without optimizations.
  `Cargo.toml` sets `[profile.dev.package.zune-jpeg] opt-level = 3`.

## Running

- Dev: `cd apps/desktop && cargo run -- <image_path>`
- Release: `cd apps/desktop && cargo run --release -- <image_path>`
- Verbose: `RUST_LOG=debug cargo run -- <image_path>`
- Target a feature: `RUST_LOG=prvw::navigation::preloader=debug ...`

## Tests

- All Rust checks: `./scripts/check.sh --rust`
- Specific test: `cd apps/desktop && cargo test <test_name>`
- Integration tests drive the QA server; see `tests/integration.rs` and `qa/CLAUDE.md`.
