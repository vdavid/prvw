# Platform: macOS (truly cross-cutting glue)

Feature-owned macOS code lives in the feature. This module only holds the glue that doesn't belong to any single
feature.

| File              | Purpose                                                                                                        |
| ----------------- | -------------------------------------------------------------------------------------------------------------- |
| `clipboard.rs`    | Copy the current image to `NSPasteboard` as a file URL + bitmap (Edit → Copy, ⌘C, right-click)                 |
| `menu_cleanup.rs` | `NSMenuDelegate` that strips AppKit's auto-injected Edit/View items (Writing Tools, Enter Full Screen, …)      |
| `open_handler.rs` | ObjC method injection of `application:openURLs:` into winit's `NSApplicationDelegate`                          |
| `print.rs`        | Print the current image via the system print sheet (File → Print, ⌘P, right-click)                             |
| `ui_common.rs`    | Shared AppKit helpers: `FlippedView`, labels, vibrancy, window centering, `as_view` cast, image + icon loaders |

The helpers are `pub(crate)` so any feature building an AppKit window can reach them without duplicating.

## Copy + Print share an image loader

`ui_common::load_image_from_path` is the single source both `clipboard.rs` and `print.rs` use to get an `NSImage`. It
loads from the **original file on disk** (ImageIO-decoded, color-managed by the file's embedded ICC profile via
ColorSync), never Prvw's in-memory decode — that buffer is already transformed to the display profile (and may be HDR
half-float), so reusing it would shift colors once another app or the printer re-interprets it. Known tradeoff: RAW
files get macOS's ImageIO rendering, not Prvw's own RAW pipeline output, so a copied/printed RAW won't match the screen.

## Decision: Print runs as a window-modal sheet, not `runOperation`

**Decision:** `print.rs` calls `runOperationModalForWindow:delegate:didRunSelector:contextInfo:` (an async sheet on the
viewer window), not the app-modal `runOperation`.

**Why:** `runOperation` spins a nested run loop — exactly the segfault pattern the modal rule below forbids inside
winit's event loop (Print is dispatched from a menu event, i.e. a winit callback). The sheet is driven by the existing
run loop instead. The sheet returns immediately, so `App._active_print` holds the `NSPrintOperation` alive (replaced on
the next print) for the sheet's duration. The print view is sized to one page's printable area and draws the image
aspect-fit; `aspect_fit_rect` is the pure, unit-tested core.

## Gotchas (cross-cutting)

- **Never run AppKit modals inside winit's event loop.** Nested run loops segfault on autorelease pool cleanup when an
  Apple Event drains objects from the wrong scope. Run native modals BEFORE `EventLoop::new()` (see `main()`), or defer
  via `EventLoopProxy`.
  - **Context menus are the exception and are safe inside the loop.** muda's `show_context_menu_for_nsview`
    (`App::show_image_context_menu`, called from a `MouseInput` Right event) runs NSMenu's own tracking loop, but muda
    owns the menu's lifetime, so there's no manually-managed `Retained<>` to get drained mid-track. Verified stable
    under repeated right-clicks. The modal rule is specifically about hand-built `runModalForWindow` sessions, not
    popups.
- **`Retained<>` lifetime inside long-lived windows.** Every objc2 `Retained<NSTextField/NSButton/...>` must stay alive
  for the window's lifetime. Store them in a `Vec<Retained<AnyObject>>` that outlives the window. Dropping early =
  segfault in autorelease pool cleanup. No compile-time check.
- **`define_class!` methods get an implicit `_cmd: Sel`.** For plain Rust helpers, put them in a separate `impl` block
  outside the macro.
- **`msg_send!` return types must match ObjC exactly.** Mismatch → runtime panic.
- **ObjC method injection for Apple Events.** winit 0.30 registers its own `WinitApplicationDelegate` and panics if
  replaced. `open_handler::register()` uses `class_addMethod` to inject `application:openURLs:` AFTER `EventLoop::new()`
  but BEFORE `run_app()`. Later = too late (Apple Events fire during `finishLaunching`).
- **FlippedView.** winit's contentView is flipped (Y=0 at top). When you add custom subviews, use
  `FlippedView::new_as_nsview` so layout math matches.
- **Native AppKit views over/around the wgpu Metal layer** (read before adding any native chrome — labels, a sidebar, a
  toolbar — to the viewer window):
  - A transparent Metal pixel still **occludes** in-window content behind it; only the server-composited `BehindWindow`
    vibrancy blur bleeds through. So a native view _layered over_ the image area must be a **sibling of the
    `CAMetalLayer` with a higher `zPosition`** — the Metal layer sits at `1.0`
    (`window::push_metal_layer_above_vibrancy`), so the view goes at `2.0`. Putting it behind the layer, or raising a
    _parent_ vibrancy view's own `zPosition`, won't surface it: the parent's `zPosition` does not carry its subviews
    above the sibling Metal layer. For a **tiled** region like a folder-browser sidebar, prefer bounding the Metal layer
    to the image pane (no overlap to fight) — see the `NSSplitView` render-pane sketch in
    `docs/specs/native-title-overlay.md`.
  - Add window chrome in `window::configure_macos_window` **unconditionally**. `add_titlebar_vibrancy` (the legacy
    strip) is gated behind `!liquid_glass_available()`, so hanging new views off it silently skips them on macOS 26+
    Liquid Glass.
  - Worked example: the native title/zoom labels (`window::add_titlebar_labels`) and
    `docs/specs/native-title-overlay.md`.
