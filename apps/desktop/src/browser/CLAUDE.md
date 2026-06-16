# Browser (browse mode)

A second top-level screen for the main window: a native AppKit folder tree + thumbnail grid that **swaps** with the wgpu
image viewer. This module currently holds the **Phase 0 spike** — the container, swap, focus, and keyboard plumbing with
stub content. Real tree/grid land in later phases. Full design: `docs/specs/image-browser.md`.

| File            | Purpose                                                                                      |
| --------------- | -------------------------------------------------------------------------------------------- |
| `mod.rs`        | `ViewMode` + `PaneSide` enums + `browser::State` (mode, focused pane, native handles); tests |
| `split_view.rs` | macOS `NSSplitView` build, hide/show, and `set_focused_pane` highlight (no keyboard here)    |

## The swap

`App` holds `browser: browser::State`. `App::set_view_mode` (in `app.rs`) drives it:

- **Image → Browse:** build the split view on first use, unhide it, `window::set_metal_layer_hidden(true)`, focus the
  tree pane. No redraw requested — the GPU goes idle (render-on-demand).
- **Browse → Image:** hide the split view, `set_metal_layer_hidden(false)`, `request_redraw()`.

The split view is a **sibling subview of winit's contentView** at `zPosition` 2.0 (above the Metal layer's 1.0), pinned
to all four edges, identifier `prvw.browser_split`, hidden at startup. Same pattern as `window::add_titlebar_labels`. A
transparent Metal pixel occludes content behind it, so the native UI must sit in front, not behind — hence
hide-one-show-the-other, not compositing.

Commands: `ToggleBrowseMode` (menu + Enter in image mode), `EnterImageMode` (Esc/Enter while browsing),
`ToggleBrowseFocus` (Tab while browsing). Dispatched in `app/executor.rs`.

## Focus / keyboard (the spike's reason to exist)

**Established finding (live test):** when the native split view is shown, **winit keeps delivering
`WindowEvent::KeyboardInput`** — the native pane's `keyDown:` override never fires, even though `makeFirstResponder`
returns `true` (winit re-asserts its content view as first responder). So browse-mode keyboard does **not** use the
AppKit responder chain.

**The input model:** all keys flow through winit → `input` → `AppCommand`, **branched by mode**. The
`WindowEvent::KeyboardInput` handler (`app.rs`) and the QA `SendKey` handler (`app/executor.rs`) both call
`input::browse_key_to_command` / `input::browse_qa_key_to_command` when `browser.is_browse()`, else the image-mode
mappings (image mode is byte-for-byte unchanged). Browse keys: Esc/Enter → `EnterImageMode`; Tab → `ToggleBrowseFocus`.
The `main.rs`/executor Esc special-case (fullscreen-or-quit, `AppCommand::Exit`) is never reached in browse mode because
Esc maps to `EnterImageMode` before it, so Esc returns to image mode and never quits.

**Focused pane is app-tracked,** not the native key-view loop: `browser::State::focused_pane`
(`PaneSide::{Tree, Grid}`). `ToggleBrowseFocus` flips it and calls `split_view::set_focused_pane`, which recolors the
panes so the focused one is highlighted. The native views will render their own selection regardless of first-responder
state, so this app-managed focus is enough (Phase 3/4). `SharedAppState` exposes `view_mode` + `focused_pane` (also at
`GET /state`) so QA/tests can assert the mode swap and focus flip without real keystrokes.

## Gotchas

- **`Retained<>` must outlive the window.** `BrowseSplitView` stores the split view + both panes; the view hierarchy
  also retains them after `addSubview`. Dropping early segfaults the autorelease pool (no compile-time check) — see
  `platform/macos/CLAUDE.md`.
- **`NSSplitView` sizes its arranged subviews itself.** The panes KEEP `translatesAutoresizingMaskIntoConstraints` ON
  (the default). Disabling it on the arranged subviews (and giving them no size constraints) collapses both panes to
  zero — the gray void the first spike rendered. Set an initial divider position with `setPosition:ofDividerAtIndex:` so
  neither pane starts collapsed.
- **Build on the main thread.** `BrowseSplitView::create` asserts the `MainThreadMarker`; it's only ever called from the
  winit event loop (main thread).
