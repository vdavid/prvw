# Native UI (AppKit secondary windows)

Non-modal NSWindow UIs built with objc2: About, Onboarding, Settings. Each window lives
in its own submodule. Shared view factories and window chrome helpers live in
`native_ui.rs` (the module root).

| File             | Purpose                                                      |
| ---------------- | ------------------------------------------------------------ |
| `native_ui.rs`   | Module root: `FlippedView`, label/button/vibrancy factories, window centering, app-icon loader |
| `about.rs`       | About window (app icon, version, links)                      |
| `onboarding.rs`  | Onboarding window (shown when launched without a file), `OnboardingState` + timer-driven re-render |
| `settings.rs`    | Settings window (sidebar + four panels), `SettingsDelegate`, per-UTI file-association toggles |

## Key patterns

- **All windows are non-modal.** `makeKeyAndOrderFront` + `mem::forget` the retained views
  (or push them to a leaked `Vec`). A dedup guard (`is_window_already_open`) prevents
  stacking.
- **Retained-mode UI.** Settings builds all four panels once; section switching uses
  `setHidden:` to show/hide pre-built panels. Dynamic text updates in place via stored
  `NSTextField` pointers in `SettingsDelegateIvars`.
- **Toggles apply immediately** via `AppCommand` through the global event loop proxy
  (see `commands::send_command`). No confirm/apply step. The button is "Close", not "OK".
- **Cross-dependencies between toggles** (ICC off disables Color match display + Relative
  colorimetric; Auto-fit on disables Enlarge small images) are handled in the delegate
  methods by `setEnabled:` on the dependent toggle pointer.
- **Onboarding is non-modal so Apple Events still dispatch.** Finder double-click must
  reach the viewer while onboarding is visible. `OnboardingState` is pure data; an
  `NSTimer` polls every second and calls `OnboardingUI::render()`.

## Visibility

Helpers in `native_ui.rs` are `pub(super)` — available to the submodules (`about`,
`onboarding`, `settings`) but not leaked to the rest of the crate. Only the window
entry points (`show_*_window`, `close_*_window`, `switch_settings_section`) are
re-exported via `pub use`.

## Gotchas

- **FlippedView.** Use `FlippedView::new_as_nsview(mtm)` instead of `NSView::new(mtm)`
  for custom container views. winit's contentView is flipped (Y=0 at top), so matching
  that coordinate system avoids NSScrollView bottom-anchoring surprises.
- **`Retained<>` lifetime inside modal/long-lived windows.** Every NSTextField/NSButton/
  NSSwitch/etc. must stay in a `Vec<Retained<AnyObject>>` that lives alongside the window.
  Dropping early = segfault in autorelease pool cleanup. No compile-time check.
- **Never run AppKit modals from inside winit's event loop.** Creates a nested run loop
  that segfaults on autorelease pool cleanup. Run native modals BEFORE `EventLoop::new()`.
- **`define_class!` methods get an implicit `_cmd: Sel`.** Plain helper methods inside
  `define_class!` are treated as ObjC methods and receive an implicit selector argument.
  For plain Rust helpers, use a separate `impl` block outside the macro.
- **Raw `msg_send!` for CoreGraphics opaque types.** `CGColorRef` and `CGColorSpaceRef`
  are `*const c_void`, which `msg_send!` encodes as `^v`. ObjC expects `^{CGColor=}`.
  Use `objc2::ffi::objc_msgSend` + `transmute`. See the separator color code in
  `settings.rs`.
- **`msg_send!` return types must match ObjC exactly.** `setActivationPolicy:` returns
  `BOOL`, not `void`. Mismatch panics at runtime.

## How to add a new setting

1. `settings.rs` struct — add the field with `#[serde(default)]`, update `Default` + tests.
2. `App` struct (`main.rs`) — add a field, initialize from `initial_settings`.
3. `AppCommand` (`commands.rs`) — add a `Set{Name}(bool)` variant.
4. `execute_command` (`main.rs`) — handle it: update the App field, load/save Settings,
   sync menu checkmark, call `self.update_shared_state()`.
5. Menu item (optional) — `menu.rs` (MenuIds + CheckMenuItem), `input.rs` (menu_to_command),
   `handle_menu_event` in `main.rs`.
6. Settings toggle — `native_ui/settings.rs`: use `make_setting_row()`. Add action method
   to `SettingsDelegate`. Store the toggle pointer in `SettingsDelegateIvars` if other
   code needs to enable/disable it. Push all created views to `retained_views`.
7. QA/MCP — HTTP endpoint + MCP tool in `qa_server.rs`.
8. Integration test — `tests/integration.rs`.
