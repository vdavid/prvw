# Platform — macOS

Everything that touches AppKit, CoreGraphics, LaunchServices, or ObjC runtime lives
here. Callers outside this subtree reach in via `crate::platform::macos::*`, which is
already gated `#[cfg(target_os = "macos")]` at the module root.

| File                   | Purpose                                                                     |
| ---------------------- | --------------------------------------------------------------------------- |
| `display_profile.rs`   | Display ICC detection via `CGDisplayCopyColorSpace`, `CAMetalLayer` colorspace, screen-change observer |
| `file_associations.rs` | `LSSetDefaultRoleHandlerForContentType` FFI for "Set Prvw as default viewer" |
| `native_ui/`           | AppKit secondary windows (About, Onboarding, Settings) — see its own CLAUDE.md |
| `open_handler.rs`      | ObjC runtime method injection of `application:openURLs:` into winit's delegate |
| `updater.rs`           | Auto-update check (background thread, GitHub releases)                       |

## Key patterns

- **ObjC method injection for Apple Events.** winit 0.30 registers its own
  `WinitApplicationDelegate` and panics if replaced. It also doesn't implement
  `application:openURLs:`, so Finder double-clicks fall through to `NSDocumentController`
  ("cannot open files in X format"). `open_handler::register()` uses `class_addMethod`
  to inject `openURLs:` into winit's delegate AFTER `EventLoop::new()` but BEFORE
  `run_app()`. Registering in `resumed()` is too late — the Apple Event fires during
  `finishLaunching`.
- **CoreGraphics via raw ObjC.** `msg_send!` panics on `CGColorRef` / `CGColorSpaceRef`
  because they're `*const c_void` (encoded as `^v`) while ObjC expects `^{CGColor=}`.
  Use `objc2::ffi::objc_msgSend` + `std::mem::transmute`. See `display_profile.rs` and
  the separator color code in `native_ui/settings.rs`.

## Gotchas

- **Never run AppKit modals inside winit's event loop.** Nested run loops segfault on
  autorelease pool cleanup when an Apple Event drains objects from the wrong scope.
  Run native modals BEFORE `EventLoop::new()` (see `main()` → pre-launch dialogs), or
  defer the modal to after the event loop exits via `EventLoopProxy`.
- **`Retained<>` lifetime inside modal/long-lived windows.** Keep every objc2
  `Retained<NSTextField/NSButton/...>` alive in a `Vec` that outlives the window.
  Dropping early = segfault in autorelease pool cleanup. No compile-time check.
- **`define_class!` methods get an implicit `_cmd: Sel` parameter.** Plain helper
  methods inside `define_class!` get treated as ObjC methods with an extra selector
  argument. For plain Rust helpers, put them in a separate `impl` block.
- **`msg_send!` return types must match ObjC exactly.** `setActivationPolicy:` returns
  `BOOL`, not `void`. Mismatch → runtime panic.
- **`request_inner_size` is async on macOS.** After calling it, `window.inner_size()`
  still returns the old size; the `Resized` event arrives later.
- **`CAMetalLayer` is a sublayer, not the NSView's direct layer.** `[ns_view layer]`
  returns the root `CALayer`; the Metal layer is in `[[ns_view layer] sublayers]`
  (typically index 0). `set_layer_colorspace` handles this with a
  `respondsToSelector:` check and sublayer walk.
- **macOS layer compositing gotcha.** `addSubview:` promotes the subview's `CALayer`
  to a sublayer of the parent's `CALayer`, placed AFTER any layers added via
  `addSublayer` regardless of `positioned:`. Use `zPosition` to override ordering.
  This is how the wgpu `CAMetalLayer` (added directly) ends up behind
  `NSVisualEffectView` subview layers and needs `zPosition = 1` to come forward.
- **winit's NSView is flipped (isFlipped = YES).** Y=0 at top. Plain AppKit NSViews
  default to bottom-left origin. Manual frame math places things at the wrong end;
  prefer Auto Layout (`.Top` / `.Leading` anchors).
- **Display profile fallback.** If `CGDisplayCopyColorSpace` / `CGColorSpaceCopyICCData`
  returns null (headless, SSH, CI), falls back to
  `/System/Library/ColorSync/Profiles/sRGB Profile.icc` via `color::srgb_icc_bytes()`.
