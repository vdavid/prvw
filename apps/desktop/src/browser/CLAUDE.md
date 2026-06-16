# Browser (browse mode)

A second top-level screen for the main window: a native AppKit folder tree + thumbnail grid that
**swaps** with the wgpu image viewer. This module currently holds the **Phase 0 spike** — the
container, swap, focus, and keyboard plumbing with stub content. Real tree/grid land in later
phases. Full design: `docs/specs/image-browser.md`.

| File            | Purpose                                                                            |
| --------------- | ---------------------------------------------------------------------------------- |
| `mod.rs`        | `ViewMode` enum + `browser::State` (mode + native handles); pure mode-toggle tests |
| `split_view.rs` | macOS `NSSplitView` build, hide/show, focus, and the `BrowsePane` keyDown handler  |

## The swap

`App` holds `browser: browser::State`. `App::set_view_mode` (in `app.rs`) drives it:

- **Image → Browse:** build the split view on first use, unhide it, `window::set_metal_layer_hidden(true)`, focus the tree pane. No redraw requested — the GPU goes idle (render-on-demand).
- **Browse → Image:** hide the split view, `set_metal_layer_hidden(false)`, `request_redraw()`.

The split view is a **sibling subview of winit's contentView** at `zPosition` 2.0 (above the Metal
layer's 1.0), pinned to all four edges, identifier `prvw.browser_split`, hidden at startup. Same
pattern as `window::add_titlebar_labels`. A transparent Metal pixel occludes content behind it, so
the native UI must sit in front, not behind — hence hide-one-show-the-other, not compositing.

Commands: `ToggleBrowseMode` (menu + Enter in image mode), `EnterImageMode` (Esc/Enter inside a
pane). Dispatched in `app/executor.rs`.

## Focus / keyboard (the spike's reason to exist)

**Finding (spike):** the swap, Metal-layer hide/show, and `makeFirstResponder:` on the tree pane
all work with zero crashes; `makeFirstResponder` returns `true`. Verified by driving
`ToggleBrowseMode` through the QA server and reading the logs.

**Still open (needs live human verification):** whether winit's `WindowEvent::KeyboardInput` keeps
firing once a native pane is first responder. The QA `/key` path injects `AppCommand`s directly, so
it can't answer this — only a focused window with real OS keystrokes can. The design assumes winit
goes quiet for keys a focused AppKit view consumes, so `BrowsePane` (an `NSScrollView` subclass)
overrides `keyDown:` as the native route: Tab → focus the other pane via the window's
`makeFirstResponder:`; Enter/Esc → `EnterImageMode`. `acceptsFirstResponder` → true so a pane can
hold focus. If the live check shows winit *does* still see the keys, the override is belt-and-braces
(harmless); if it doesn't, the override is load-bearing.

## Gotchas

- **`Retained<>` must outlive the window.** `BrowseSplitView` stores the split view + both panes;
  the view hierarchy also retains them after `addSubview`. Dropping early segfaults the autorelease
  pool (no compile-time check) — see `platform/macos/CLAUDE.md`.
- **`keyCode` is a hardware code, not a character.** `BrowsePane::key_down` matches Tab=48,
  Return=36, keypad Enter=76, Escape=53. Unhandled codes fall through to `super` so AppKit keeps
  arrow/selection behavior inside a pane.
- **Build on the main thread.** `BrowseSplitView::create` asserts the `MainThreadMarker`; it's only
  ever called from the winit event loop (main thread).
