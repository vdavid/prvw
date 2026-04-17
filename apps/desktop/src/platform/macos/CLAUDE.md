# Platform — macOS (truly cross-cutting glue)

Feature-owned macOS code lives in the feature. This module only holds the glue that
doesn't belong to any single feature.

| File             | Purpose                                                                           |
| ---------------- | --------------------------------------------------------------------------------- |
| `open_handler.rs` | ObjC method injection of `application:openURLs:` into winit's `NSApplicationDelegate` |
| `ui_common.rs`   | Shared AppKit helpers: `FlippedView`, labels, vibrancy, window centering, `as_view` cast, app-icon loader |

The helpers are `pub(crate)` so any feature building an AppKit window can reach them
without duplicating.

## Gotchas (cross-cutting)

- **Never run AppKit modals inside winit's event loop.** Nested run loops segfault on
  autorelease pool cleanup when an Apple Event drains objects from the wrong scope.
  Run native modals BEFORE `EventLoop::new()` (see `main()`), or defer via
  `EventLoopProxy`.
- **`Retained<>` lifetime inside long-lived windows.** Every objc2
  `Retained<NSTextField/NSButton/...>` must stay alive for the window's lifetime —
  store them in a `Vec<Retained<AnyObject>>` that outlives the window. Dropping
  early = segfault in autorelease pool cleanup. No compile-time check.
- **`define_class!` methods get an implicit `_cmd: Sel`.** For plain Rust helpers,
  put them in a separate `impl` block outside the macro.
- **`msg_send!` return types must match ObjC exactly.** Mismatch → runtime panic.
- **ObjC method injection for Apple Events.** winit 0.30 registers its own
  `WinitApplicationDelegate` and panics if replaced. `open_handler::register()` uses
  `class_addMethod` to inject `application:openURLs:` AFTER `EventLoop::new()` but
  BEFORE `run_app()`. Later = too late (Apple Events fire during `finishLaunching`).
- **FlippedView.** winit's contentView is flipped (Y=0 at top). When you add custom
  subviews, use `FlippedView::new_as_nsview` so layout math matches.
