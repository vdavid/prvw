# Desktop app

Feature-based, flat layout. Each directory under `src/` is either infrastructure (used by every feature) or one
user-visible feature. No `features/` wrapper.

## Source layout

```
src/
├── main.rs                  Thin entry: CLI, logger, event-loop setup
│
│   Infrastructure:
├── app.rs + app/            App struct, ApplicationHandler, command dispatcher, shared-state snapshot
├── commands.rs              AppCommand enum + global EventLoopProxy
├── input.rs                 Maps keys/QA keys → AppCommand
├── menu/                    Menu bar + context menu (muda) on macOS and Windows; `absent.rs` covers platforms with no menu bar
├── parity/                  Registries of settings, menu items, and commands + each platform's coverage (M0.5 layer 1)
├── pixels.rs                Logical/Physical coordinate newtypes
├── platform.rs + platform/  Cross-cutting platform glue (Apple Events, AppKit helpers)
├── render.rs + render/      wgpu infrastructure (renderer, text, shaders)
│
│   Features:
├── about.rs                 About window
├── browser/                 macOS-only: browse mode — native AppKit folder tree + thumbnail grid that swaps with the wgpu viewer + browser::State
├── color/                   ICC transform + display profile (macOS) + Color settings panel + color::State
├── decoding/                Image format decoders (JPEG via zune-jpeg, RAW via rawler, generic via `image`) + `RawPipelineFlags` per-stage toggle struct + `ExifMetadata` extraction
├── diagnostics.rs           Performance observability (cache/nav/RSS formatter)
├── exif_overlay/            EXIF info overlay (toggleable read-only metadata panel below the histogram)
├── file_associations/       LaunchServices FFI + File associations settings panel
├── histogram/               256-bin RGB histogram overlay (toggle via View → Histogram or H key) + histogram::State
├── navigation/              Directory scan + preloader + LRU cache + navigation::State
├── onboarding/              Onboarding window + defaults-sentence generator + SVG checkmark renderer
├── qa/                      Embedded HTTP + MCP server
├── settings/                JSON persistence + Settings window shell + widgets + General panel + RAW panel (Phase 3.7)
├── slideshow/               Timer-driven auto-advance + crossfade + Slideshow settings panel + slideshow::State
├── previews/                macOS-only: QuickLook-backed preview preload + blurry-placeholder placeholder
├── updater.rs               Auto-update
├── window.rs                Main viewer window: create, fullscreen, auto-fit, vibrancy
└── zoom/                    ViewState + zoom/pan math + Zoom settings panel + zoom::State
```

Single-file features (`about.rs`, `diagnostics.rs`, `updater.rs`, `window.rs`) use their `//!` module docs in place of a
`CLAUDE.md`. Directory-based features have a colocated `CLAUDE.md` or rely on `//!` docs on each submodule
(`onboarding/`).

## Per-feature state

`App` holds `zoom: zoom::State`, `color: color::State`, `navigation: navigation::State`, `browser: browser::State`, and
(macOS) `previews: previews::State`. Each feature's runtime state lives in its own module. App only keeps truly
cross-cutting state: handles (window, renderer, menu), launch flags (file_path, waiting_for_file, launch_directory),
runtime input (modifiers, drag_start, etc.), and the single cross-feature toggle `title_bar`.

**Gotcha: `waiting_for_file` is a macOS-only state.** `resumed()` builds no window in it, and what makes that safe on
macOS is that Finder delivers the file through an Apple Event and `onboarding` puts a window up meanwhile. Neither
exists elsewhere, so `main` logs and exits (code 2) rather than leaving a windowless process — which is what a Windows
Start-menu or taskbar launch would otherwise produce, since those pass no argv. M1 step 1 of
`docs/specs/cross-platform-plan.md` replaces the exit with an empty-state window plus File → Open.

## Top-level principles

- **`winit` 0.30 `ApplicationHandler`.** App implements the trait. Window + wgpu surface created in `resumed()`, not
  startup (required on macOS).
- **Render on demand.** `App.needs_redraw` gates frames. No continuous render loop.
- **Command architecture.** Every user action becomes an `AppCommand` in `crate::commands`. `App::execute_command`
  (`app/executor.rs`) is the single dispatcher. Keys, menus, QA HTTP, MCP, AppKit delegates all funnel there.
- **No `tokio`.** CPU-bound decoding runs on `std::thread` via rayon. `mpsc` channels cross threads.
- **Shared-state boundary.** `SharedAppState` (in `app/shared_state.rs`) is the snapshot the QA thread reads. Main
  thread writes on every observable change; diagnostics text is rendered by `diagnostics::build_text`.

## Cross-cutting gotchas

See `platform/macos/CLAUDE.md` for the full list. Short version:

- **Never run AppKit modals inside winit's event loop.** Segfault. Run them before `EventLoop::new()` or defer via
  `EventLoopProxy`.
- **`Retained<>` outlives the window.** Store every objc2 `Retained<...>` in a `Vec` that outlives the window. No
  compile-time check.
- **Finder file opens need ObjC method injection** into winit's delegate. See `platform/macos/open_handler.rs`.
- **Native AppKit views over/around the wgpu Metal layer** (sidebar, labels, any window chrome): they must be siblings
  of the `CAMetalLayer` at a higher `zPosition` (a transparent Metal pixel still occludes content behind it), and added
  on both window paths. Read the full gotcha in `platform/macos/CLAUDE.md` before building such UI.
- **`zune-jpeg` in debug builds.** SIMD unusably slow without optimizations. The **workspace-root** `Cargo.toml` sets
  `[profile.dev.package.zune-jpeg] opt-level = 3` (Cargo ignores `[profile.*]` in member manifests).

## Running

- Dev: `cd apps/desktop && cargo run -- <image_path>`
- Release: `cd apps/desktop && cargo run --release -- <image_path>`
- Verbose: `RUST_LOG=debug cargo run -- <image_path>`
- Target a feature: `RUST_LOG=prvw::navigation::preloader=debug ...`

## Tests

- All Rust checks: `./scripts/check.sh --rust`
- Specific test: `cd apps/desktop && cargo test <test_name>`
- Integration tests drive the QA server; see `tests/integration.rs` and `qa/CLAUDE.md`.
