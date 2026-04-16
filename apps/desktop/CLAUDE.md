# Desktop app

The Prvw desktop app: a GPU-accelerated image viewer using `winit` + `wgpu` + `muda`.

## Architecture

The app struct implements `winit::application::ApplicationHandler`. The event loop drives everything.

| Module            | Responsibility                                                                            |
|-------------------|-------------------------------------------------------------------------------------------|
| `main.rs`         | CLI parsing, event loop, `ApplicationHandler` impl, command executor                      |
| `input.rs`        | Maps keyboard, mouse, menu, and QA key events to `AppCommand`s                            |
| `window.rs`       | Window creation, fullscreen toggle, auto-fit resize, title formatting                     |
| `renderer.rs`     | wgpu surface, pipeline, texture upload, rendering                                         |
| `image_loader.rs` | Decode image files to RGBA8 (zune-jpeg for JPEG, `image` crate for others), ICC extraction |
| `color.rs`        | ICC color management: transform source profile to display profile via moxcms              |
| `display_profile.rs` | macOS display ICC profile detection, CAMetalLayer colorspace, screen change observer   |
| `view.rs`         | Zoom/pan math, transform uniform for GPU                                                  |
| `menu.rs`         | Native macOS menu bar via `muda`, shortcut wiring                                         |
| `directory.rs`    | Scan parent dir for images, sort, track position                                          |
| `preloader.rs`    | Parallel background decoding (rayon pool), LRU cache (512 MB budget)                      |
| `native_ui.rs`    | AppKit secondary windows (About, Onboarding, Settings) via objc2                          |
| `onboarding.rs`   | File association queries and default viewer registration helpers                          |
| `settings.rs`     | Settings persistence (JSON file in app data dir, overridable via `PRVW_DATA_DIR` env var) |
| `qa_server.rs`    | `AppCommand` enum, embedded HTTP/MCP server for QA/E2E, global event loop proxy           |
| `shader.wgsl`     | WGSL vertex/fragment shader for textured quad with 2D transform                           |

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

- **Zoom model**: Zoom is absolute: `zoom=1.0` means 1 image pixel = 1 screen pixel. `fit_zoom()` computes the zoom that
  makes the image exactly fit the window (< 1.0 for large images, > 1.0 for small ones). The zoom floor
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

- **Coordinate conventions**: Two coordinate systems are used throughout:
    - **Logical pixels** (f64): UI layout coordinates, independent of display scaling. Used for window position
      (`outer_position`), window content size in `MonitorBounds`, `TextBlock` coordinates, and `MeasuredPill`
      positions. 1 logical pixel = 1 point on macOS.
    - **Physical pixels** (u32): Actual GPU surface pixels. Used for `surface_width()`/`surface_height()`, wgpu texture
      sizes, and `PhysicalSize` from winit. On Retina displays, 1 logical = 2 physical. The `scale_factor` (stored on
      `App`, also on `Renderer`) converts between them. `Renderer::logical_width()` is a convenience for
      `surface_width / scale_factor`. When in doubt, check the function signature: winit's `LogicalSize`
      and `PhysicalSize` types make the system explicit at API boundaries.

- **Command architecture**: All user actions are expressed as `AppCommand` variants (defined in `qa_server.rs`).
  `input.rs` maps keyboard, menu, and QA key events to commands. `App::execute_command()` in `main.rs` is the single
  place where each command's effect is implemented. Scroll zoom, mouse drag, and cursor tracking stay inline in
  `window_event` since they're continuous input, not discrete commands.

- **QA server**: An embedded HTTP server (raw `TcpListener`, no external crate) on a background thread. Agents and E2E
  tests use it to query state, send commands, and capture screenshots. Port controlled by `PRVW_QA_PORT` env var
  (default 19447, set to 0 to disable). Commands flow through `EventLoopProxy<AppCommand>` user events. Screenshots use
  an offscreen wgpu render target + buffer readback + PNG encoding.

- **Native secondary windows** (`native_ui.rs`): About, Onboarding, and Settings windows are built with AppKit via
  objc2. All use `NSStackView` for layout, `NSVisualEffectView` for frosted glass, and transparent titlebars.
    - **Onboarding** is non-modal (`makeKeyAndOrderFront`), shown after a 500ms delay when no CLI files are provided.
      This allows the event loop to receive Apple Events (Finder double-click) while onboarding is visible. Uses a
      state/render separation: `OnboardingState` (pure data) + `OnboardingUI::render()`. An `NSTimer` polls every
      second. When a file arrives via Apple Event, the onboarding closes and the viewer initializes.
    - **About and Settings** are non-modal: `makeKeyAndOrderFront` + `mem::forget` the retained views. A deduplication
      guard (`is_window_already_open`) prevents stacking. FIXME: views leak on close/reopen (see code comments).
    - **Settings** uses a sidebar + content panel layout (like macOS System Settings). Four sections: General
      (Auto-update), Zoom (Auto-fit window, Enlarge small images), Color (ICC color management, Color match display,
      Relative colorimetric), File associations (Set all + per-UTI toggles). A "section" is the entity selected in
      the sidebar. The `SettingsDelegate` (via `define_class!` with `SettingsDelegateIvars`) holds raw pointers to
      dependent toggles and all four panels. Sidebar buttons use `AccessoryBarAction` bezel style with `PushOnPushOff`
      button type. Panel switching shows/hides NSStackView panels. Cross-dependencies: ICC off disables Color match
      display and Relative colorimetric toggles; Auto-fit on disables Enlarge small images. All views (toggles,
      panels, sidebar buttons) are created first, then the delegate is created with ivars pointing to them, then
      target/action is wired. Toggles apply immediately (no confirm step) via `AppCommand` through the global event
      loop proxy. All toggle rows use a spacer view to right-align the NSSwitch to the trailing edge. Per-UTI toggles
      use `NSControlSizeSmall` for a compact appearance.
    - **Settings UI is retained-mode, not rebuilt.** The entire view tree is built once. Section switching uses
      `setHidden:` to show/hide pre-built panels (all four exist simultaneously). Dynamic text (like file association
      descriptions) is updated in place via `setStringValue:` on existing `NSTextField` labels. To add a dynamic
      description to a setting toggle, store the label pointer in `SettingsDelegateIvars` and call `setStringValue:`
      from the toggle's action handler. This is how file associations update their "Currently opens with X" text
      without rebuilding anything.

### How to add a new setting

Follow these steps in order. Each one is required.

1. **`settings.rs`**: Add the field to `Settings` struct with `#[serde(default)]` (or `default = "default_true"` if
   it should default to `true`). Update `Default` impl and tests.
2. **`App` struct in `main.rs`**: Add a field, initialize from `initial_settings` in `App::new()`.
3. **`AppCommand` in `qa_server.rs`**: Add a `Set{SettingName}(bool)` variant.
4. **`execute_command` in `main.rs`**: Add a handler that updates the App field, loads/saves Settings, syncs the menu
   checkmark if applicable, and calls `self.update_shared_state()`. If the setting has cross-dependencies (like
   auto-fit disabling enlarge), update the dependent toggle's enabled state here.
5. **Menu item** (if the setting should be in the View menu): Add to `menu.rs` (MenuIds + CheckMenuItem), wire in
   `input.rs` (menu_to_command), handle in `handle_menu_event` in `main.rs`.
6. **Settings window toggle in `native_ui.rs`**: Use `make_setting_row()` to create the toggle row. Add it to the
   appropriate section panel's vertical stack. Add the toggle action method to `SettingsDelegate`. Store the toggle
   pointer in `SettingsDelegateIvars` if other code needs to enable/disable it. Push ALL created views to
   `retained_views`.
7. **MCP/QA**: Add HTTP endpoint and MCP tool in `qa_server.rs`.
8. **Integration test**: Add a test in `tests/integration.rs`.

**UX principles for settings:**
- Settings apply **immediately** on toggle — no confirm/apply button. The button is "Close", not "OK".
- Toggles are **right-aligned** via a spacer view with low content hugging priority.
- When one setting disables another (cross-dependency), gray out the dependent toggle via `setEnabled:`.
- Choose the section by domain: General (app behavior), Zoom (view behavior), Color (rendering), File associations
  (OS integration).
- New sections: add a sidebar button + panel + delegate method. Update `switch_settings_section()`.

- **Title bar area** (`view.rs` + `renderer.rs` + `main.rs`): When the `title_bar` setting is on, a 32px area at
  the top of the window is reserved (image doesn't render there). Implementation:
    - `ViewState.content_offset_y` stores the offset. `effective_height() = window_height - content_offset_y` is used
      by `fit_zoom`, `transform`, `pan`, `clamp_pan`, `zoom_around`, `keyboard_zoom` — every image-area calculation.
    - The renderer's image draw is wrapped in `set_viewport(0, offset_px, sw, sh - offset_px)` to clip the image to
      the lower area. The viewport is **reset to the full surface before pills/text** (so they can render in the
      title bar area). The offset matters because `set_viewport` REMAPS NDC [-1,1] to the viewport rectangle (not
      just clips) — so the transform's denominator must be `effective_height` for sy=1.0 to fill the viewport correctly.
    - `apply_content_offset()` in `main.rs` sets the ViewState offset, resizes the window (when auto-fit is on, by
      calling `resize_to_fit_image` with the new offset), and re-applies zoom. Without the resize, toggling the
      setting leaves the window the wrong size and produces visible padding.
    - Mouse Y for zoom-at-cursor must subtract `content_offset_y` because `zoom_around` expects coordinates relative
      to the image area, not the window.
    - In fullscreen, the offset is forced to 0 (`content_offset_y()` checks `is_fullscreen`).

- **Screenshot render path differs from main render** (`renderer.rs`): `capture_screenshot` runs a separate, stripped
  render — only the image quad on a black background, no viewport offset, no pills, no text. Pixel-based tests of the
  window's actual appearance won't work via screenshot. The screenshot test for the title bar area passes for the
  OFF state (image fills the surface) but can't verify the ON state's viewport clipping. To make screenshots match the
  window, factor out a shared inner render function that both paths call.

- **FlippedView**: Always use `FlippedView::new_as_nsview(mtm)` instead of `NSView::new(mtm)` for custom container
  views in `native_ui.rs`. macOS puts Y=0 at the bottom (Cartesian), which causes NSScrollView to bottom-anchor
  content. FlippedView overrides `isFlipped` to return `true` (Y=0 at top, like iOS/CSS/SwiftUI), making layout
  predictable. Defined at the top of `native_ui.rs`.

- **Global event loop proxy** (`qa_server.rs`): A `OnceLock<EventLoopProxy<AppCommand>>` is set once in `resumed()`.
  This lets non-event-loop code (like the native Settings delegate) send commands into the main loop. Used by
  `send_command()` — the same mechanism the QA server uses, just without needing a reference to the proxy.

- **File associations** (`onboarding.rs`): Uses `LSCopyDefaultRoleHandlerForContentType` and
  `LSSetDefaultRoleHandlerForContentType` via objc2-core-services FFI. No Swift scripts — direct C calls, near-instant.

- **Display ICC lifecycle**: The display's ICC bytes flow through: `CGDisplayCopyColorSpace` (at startup in
  `initialize_viewer`) -> stored as `App.display_icc: Vec<u8>` -> cloned into `Preloader` (as `Arc<Vec<u8>>`) -> cloned
  into each rayon task closure -> passed to `image_loader::load_image_cancellable(path, cancelled, &display_icc)` ->
  passed to `decode_jpeg`/`decode_generic` -> `color::transform_icc(rgba, source_icc, target_icc)`. On display change:
  `NSWindowDidChangeScreenNotification` -> `AppCommand::DisplayChanged` -> `handle_display_changed()` re-queries
  CoreGraphics, updates `App.display_icc`, calls `set_layer_colorspace`, flushes the image cache, updates the preloader's
  ICC copy, and re-displays the current image. The direct `display_image()` path (first image, navigate from cache miss)
  reads `self.display_icc` directly.

- **ICC color management** (`color.rs` + `display_profile.rs`): Converts images with embedded ICC profiles to the
  display's color space before GPU upload. Extraction is format-specific (in `image_loader.rs`), transform is
  format-agnostic (in `color.rs` via moxcms), display profile detection is in `display_profile.rs`. Key choices:
    - **moxcms** (pure Rust, NEON SIMD on Apple Silicon) — 5.5x faster than lcms2 for the transform step. Entire API
      surface is isolated in `color.rs` (~70 lines). The `in_place` feature flag is required for in-place transforms.
    - **Perceptual** rendering intent — maps out-of-gamut colors smoothly, which is what viewers should do.
    - **Byte-equality skip** — if source ICC bytes match target ICC bytes, the transform is skipped (zero cost for
      P3-on-P3, sRGB-on-sRGB, etc.). Images without an embedded profile are assumed sRGB.
    - This is **Level 2** (source → display profile). Level 1 was source → sRGB.
    - **Display profile detection**: `CGDisplayCopyColorSpace()` + `CGColorSpaceCopyICCData()` via CoreGraphics FFI.
      Gets the `CGDirectDisplayID` from `[[NSWindow screen] deviceDescription][@"NSScreenNumber"]` — the authoritative
      source that `NSWindowDidChangeScreenNotification` updates. Don't use winit's `current_monitor()` + position
      matching — it's unreliable and silently falls back to the main display.
    - **CAMetalLayer colorspace**: Set via `[layer setColorspace:]` so the macOS compositor knows our output color space.
      This avoids changing the texture format (`Rgba8UnormSrgb`) or shader, because P3 and sRGB share the same EOTF.
    - **Screen change detection**: `NSWindowDidChangeScreenNotification` observer fires `AppCommand::DisplayChanged`,
      which re-queries the display profile, flushes the image cache, and re-decodes the current image.
    - **Menu toggles**: Two View menu items control ICC behavior. "ICC color management" (Cmd+Shift+I) toggles L1
      on/off — when off, no transforms run and `target_icc` is empty (check in `transform_icc`). "Color match display"
      (Cmd+Shift+C) toggles L2 — when off, target is sRGB; when on, target is the display profile. The L2 toggle is
      grayed out when L1 is off. Both are persisted in `settings.json`. The shared logic lives in `effective_display_icc()`
      (computes the target ICC bytes from both flags) and `apply_icc_settings()` (sets the layer colorspace, flushes
      cache, re-decodes). `DisplayChanged` also calls `apply_icc_settings()`.
    - Full decision log with all 8 decisions and evidence: [docs/notes/icc-level-2-display-color-management.md](../../docs/notes/icc-level-2-display-color-management.md)
    - **Why moxcms over lcms2** (decided 2026-04-15):

      |                    | `lcms2` 6.1.1                  | `moxcms` 0.8.1 (chosen)              |
      |--------------------|--------------------------------|--------------------------------------|
      | Language           | C bindings (lcms2-sys bundles) | Pure Rust                            |
      | SIMD               | None (scalar C)                | NEON (ARM), AVX2/SSE4.1 (x86)       |
      | GitHub             | kornelski/rust-lcms2, 49 stars  | awxkee/moxcms, 43 stars              |
      | License            | MIT                            | BSD-3-Clause                         |
      | ICC transform 24MP | **247ms**                      | **45ms** (5.5x faster)               |
      | Total decode 24MP  | 452ms (ICC = 55%)              | 263ms (ICC = 17%)                    |
      | CMYK support       | Yes (via C lib)                | Yes                                  |
      | Maturity           | 10y (C lib: 20y+)              | 14 months, 30 releases               |
      | Cross-compile      | Needs C toolchain              | Just `cargo build`                   |

      lcms2 is more battle-tested with exotic ICC profiles, but for standard RGB profiles (Adobe RGB, ProPhoto,
      Display P3) moxcms produces identical results (verified by regression tests). The 5.5x speed advantage on
      Apple Silicon and the simpler pure-Rust build won it.

    - **Performance** (benchmarked 2026-04-15, release build, Apple M3 Max, 24MP / 6000x4000 Adobe RGB JPEG): ICC
      transform ~45ms, total decode ~263ms (JPEG decode ~218ms + ICC ~45ms). The ICC portion is ~17% of total load
      time. To reproduce:
      ```
      mkdir -p /tmp/icc-bench
      for i in $(seq -w 1 10); do
        magick -size 6000x4000 plasma:red-green -seed $i \
          -profile /System/Library/ColorSync/Profiles/AdobeRGB1998.icc \
          /tmp/icc-bench/photo_${i}.jpg
      done
      RUST_LOG=prvw::color=debug,prvw::display_profile=info,prvw::image_loader=info ./target/release/prvw /tmp/icc-bench/photo_01.jpg
      ```

## Gotchas

- **Finder file opens require ObjC runtime method injection.** Winit 0.30 registers its own `NSApplicationDelegate`
  and panics if replaced. But winit doesn't implement `application:openURLs:`, so AppKit falls through to
  `NSDocumentController` which shows "cannot open files in X format." Fix: use `ffi::class_addMethod` to inject
  `application:openURLs:` into `WinitApplicationDelegate` after `EventLoop::new()` but before `run_app()`. See
  `macos_open_handler.rs`. Registering in `resumed()` is too late — the Apple Event is dispatched during
  `finishLaunching`.
- **objc2 `msg_send!` panics on CoreGraphics opaque types.** `CGColorRef` and `CGColorSpaceRef` are `*const c_void`
  which `msg_send!` encodes as `^v`, but ObjC expects `^{CGColor=}` or `^{CGColorSpace=}`. Use raw
  `objc2::ffi::objc_msgSend` with `std::mem::transmute` to bypass the type encoding check. See `display_profile.rs`
  and the separator color code in `native_ui.rs`.
- **wgpu 29 API changes**: `Instance::new()` takes a value (not reference). `get_current_texture()` returns
  `CurrentSurfaceTexture` enum (not `Result`). `PipelineLayoutDescriptor` uses `immediate_size` instead of
  `push_constant_ranges`. `RenderPassColorAttachment` requires `depth_slice`. `mipmap_filter` uses `MipmapFilterMode`.
- **winit 0.30 `ApplicationHandler`**: No closure-based `run`. The app struct implements the trait. State that depends
  on the window (renderer, surface) must be `Option` and initialized in `resumed()`.
- **muda menu**: `init_for_nsapp()` must be called after building the menu. Menu events are polled via
  `MenuEvent::receiver().try_recv()`, not callbacks.
- **bytemuck derives**: Use `bytemuck::Pod` and `bytemuck::Zeroable` (from the `derive` feature), not
  `bytemuck_derive::Pod` directly.
- **zune-jpeg in debug builds**: zune-jpeg's SIMD is painfully slow without optimizations. `Cargo.toml` sets
  `[profile.dev.package.zune-jpeg] opt-level = 3` to fix this.
- **objc2 `Retained<>` lifetime with AppKit modals**: when creating AppKit views (NSTextField, NSButton, etc.) via objc2
  and adding them to a parent view with `addSubview`, the Rust `Retained<>` wrapper must stay alive for the entire
  duration of the modal session. If it drops (goes out of scope), AppKit's autorelease pool cleanup will segfault (
  use-after-free). Fix: collect all views in a `Vec<Retained<...>>` that lives alongside the modal loop. This applies to
  `native_ui.rs` and any future native macOS dialogs. There is no compile-time check for this.
- **Never run AppKit modals from inside winit's event loop.** Running `NSApplication::runModalForWindow` inside winit's
  `resumed()` or `window_event()` creates a nested run loop inside winit's autorelease pool. When the modal ends and an
  Apple Event arrives, the pool drains objects from the wrong scope, causing segfault. Fix: run native modals BEFORE
  `EventLoop::new()` (like the onboarding dialog in `main()`), or use `EventLoopProxy` to defer the modal to after the
  event loop exits.
- **`define_class!` methods get an implicit `_cmd: Sel` parameter.** Plain helper methods defined inside
  `define_class!` are treated as ObjC methods and receive an implicit selector argument. To define a plain Rust helper,
  put it in a separate `impl` block outside the macro, or use a free function.
- **`request_inner_size` is async on macOS.** After calling `window.request_inner_size()`, `window.inner_size()`
  still returns the OLD size. The `Resized` event arrives later. To avoid a frame of wrong proportions,
  `resize_to_fit_image` computes and returns the physical size so callers can pass it directly to `renderer.resize()`.
- **`msg_send!` return types must match the ObjC method signature exactly.** `setActivationPolicy:` returns `BOOL`, not
  `void`. Writing `let _: () = msg_send![...]` for a method that returns `BOOL` panics at runtime with
  "expected return to have type code 'B', but found 'v'". Always check Apple's docs for the return type.
- **wgpu's CAMetalLayer is a sublayer, not the view's direct layer.** When calling `[ns_view layer]`, you get the
  NSView's root `CALayer`, not the `CAMetalLayer` that wgpu created. The Metal layer is in `[[ns_view layer] sublayers]`
  (typically index 0). `set_layer_colorspace` handles this by checking `respondsToSelector:setColorspace:` and searching
  sublayers if the root layer doesn't respond. Without this, `msg_send![layer, setColorspace:]` panics inside winit's
  ObjC event loop (which aborts because panics can't unwind through `extern "C"` boundaries).
- **Display profile falls back to sRGB.** If `CGDisplayCopyColorSpace` or `CGColorSpaceCopyICCData` returns null (headless,
  SSH, CI), the display ICC defaults to the macOS system sRGB profile at `/System/Library/ColorSync/Profiles/sRGB Profile.icc`.
  The `srgb_icc_bytes()` function in `color.rs` panics if this file is missing — it's always present on macOS but won't
  exist on other platforms. Cross-platform support will need a fallback embedded sRGB profile.
- **ICC extraction ordering with the `image` crate.** `ImageReader::into_decoder()` returns `impl ImageDecoder`.
  `icc_profile()` takes `&mut self`, while `DynamicImage::from_decoder()` consumes the decoder. So you must call
  `icc_profile()` first, then `from_decoder()`. Reversing the order won't compile.
- **Screenshot surface format.** The render pipeline targets `Bgra8UnormSrgb` (macOS surface format). The screenshot
  readback copies raw BGRA bytes, which must be swizzled to RGBA before PNG encoding. If you change the surface
  format, update the swizzle in `capture_screenshot()`.

## Dependencies

| Crate                 | Version | Purpose                                                                     |
|-----------------------|---------|-----------------------------------------------------------------------------|
| winit                 | 0.30.13 | Windowing and event handling                                                |
| wgpu                  | 29.0.1  | GPU rendering (Metal on macOS)                                              |
| pollster              | 0.4.0   | Block on wgpu async calls                                                   |
| muda                  | 0.17.2  | Native macOS menu bar                                                       |
| image                 | 0.25.10 | Image decoding (PNG, GIF, WebP, BMP, TIFF) and PNG encoding for screenshots |
| zune-jpeg             | 0.5.15  | Fast JPEG decoding with SIMD (replaces `image` for JPEG)                    |
| zune-core             | 0.5.1   | Decoder options for zune-jpeg                                               |
| moxcms                | 0.8.1   | ICC color management, pure Rust with NEON SIMD (Adobe RGB/ProPhoto → sRGB)  |
| rayon                 | 1.11.0  | Thread pool for parallel preloading                                         |
| clap                  | 4.6.0   | CLI argument parsing                                                        |
| log                   | 0.4.29  | Logging facade                                                              |
| env_logger            | 0.11.10 | Log output to stderr                                                        |
| bytemuck              | 1.25.0  | Safe transmute for GPU uniform data                                         |
| objc2-core-foundation | 0.3     | CFString for CoreServices FFI                                               |
| objc2-core-services   | 0.3     | File association APIs (LSSetDefaultRoleHandler, etc.)                       |
