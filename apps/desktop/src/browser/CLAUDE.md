# Browser (browse mode)

A second top-level screen for the main window: a native AppKit folder tree + thumbnail grid that **swaps** with the wgpu
image viewer. The **tree is real** (Phase 3): an `NSOutlineView` source list of the home folder + mounted volumes. The
grid is still a `(grid)` stub (a later phase wires `NSCollectionView`). Full design: `docs/specs/image-browser.md`.

| File            | Purpose                                                                                                                                             |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `mod.rs`        | `ViewMode` + `PaneSide` enums + `browser::State` (mode, focused pane, selected folder, native handles); tree-nav delegation; tests                  |
| `split_view.rs` | macOS `NSSplitView` build, hide/show, `set_focused_pane` highlight, divider + traffic-light fixes; hosts the tree                                   |
| `outline.rs`    | macOS `NSOutlineView` source-list tree: `NodeObject`, `TreeDataSource` (data source + delegate), `BrowseTree` (owns the view + drives keyboard nav) |
| `tree_model.rs` | Pure, headless-tested logic: `child_directories`, `enumerate_roots`/`build_roots`, `count_supported_images`, `next_selectable_row`                  |

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
`ToggleBrowseFocus` (Tab while browsing), `BrowseSelectFolder(PathBuf)` (tree selection), `BrowseMoveTreeSelection(i32)`
(Up/Down), `BrowseExpandTreeSelection(bool)` (Right/Left). Dispatched in `app/executor.rs`. The `BrowseSelectFolder`,
`BrowseMoveTreeSelection`, and `BrowseExpandTreeSelection` variants are macOS-only.

## The folder tree (Phase 3)

`outline.rs` builds the left pane: an `NSOutlineView` with `setStyle(NSTableViewStyle::SourceList)` (the rounded-pill
source-list selection + inset; `setSelectionHighlightStyle:` is deprecated) inside an `NSVisualEffectView` `.sidebar`
material. One column, directories only, lazy.

- **Item identity is load-bearing.** `NSOutlineView` tracks items by pointer identity — it hands the data-source methods
  back the exact pointers they returned earlier and compares by address. So each node is a `NodeObject` (a tiny
  `NSObject` subclass holding the node's `PathBuf`), and `TreeDataSource::node_for_path` returns the **same**
  `Retained<NodeObject>` per path from a `RefCell<HashMap<PathBuf, Retained<NodeObject>>>` cache. Return a fresh object
  per call and the tree misbehaves (wrong expansion, lost selection). Child directories are likewise computed once per
  path and cached (`children` map) — the outline view queries a lot during layout.
- **Roots are flat, not grouped.** Home folder first, then each mounted volume as a sibling top-level row. A
  Finder-style "Locations" group header needs group-row pseudo-items (no path), which fights the path-keyed node model —
  so flat roots. Volumes come from `NSFileManager mountedVolumeURLsIncludingResourceValuesForKeys:options:`
  (skip-hidden), with each volume's localized name via `NSURLVolumeLocalizedNameKey`; falls back to listing `/Volumes`.
  Row order/labels are decided by the pure `tree_model::build_roots` (unit-tested); `enumerate_roots` is the macOS glue
  that feeds it.
- **Arrow keys drive the view programmatically** (winit owns the keyboard — see below): `BrowseTree::move_selection`
  (`selectRowIndexes:` after `next_selectable_row` math), `expand_selected` / `collapse_selected` (`expandItem:` /
  `collapseItem:`, Left on a leaf collapses the parent). `browser::State` gates these on the tree pane being focused.
- **Selection → app state.** `outlineViewSelectionDidChange:` sends `BrowseSelectFolder(path)`; the executor stores it
  in `State::selected_folder` and logs the supported-image count (`tree_model::count_supported_images`). The grid that
  lists those images is a later phase.

### Two fixes baked into `split_view.rs`

- **Divider opens at ~240pt without a drag.** `setPosition:ofDividerAtIndex:` no-ops at build time (zero frame). We set
  it once in `set_hidden(false)` after `layoutSubtreeIfNeeded` forces the edge constraints to resolve, latched by a
  `Cell<bool>` ivar so a later show won't yank a divider the user dragged. (An earlier attempt set it from an overridden
  `layout` — `setPosition:` re-enters `layout` synchronously, so it must not run there.)
- **Sidebar clears the traffic lights.** The `.sidebar` vibrancy fills the pane, but the outline scroll view is inset
  `crate::TITLE_BAR_HEIGHT` (32pt) from the top so no row sits under the traffic-light strip.

## Focus / keyboard (the spike's reason to exist)

**Established finding (live test):** when the native split view is shown, **winit keeps delivering
`WindowEvent::KeyboardInput`** — the native pane's `keyDown:` override never fires, even though `makeFirstResponder`
returns `true` (winit re-asserts its content view as first responder). So browse-mode keyboard does **not** use the
AppKit responder chain.

**The input model:** all keys flow through winit → `input` → `AppCommand`, **branched by mode**. The
`WindowEvent::KeyboardInput` handler (`app.rs`) and the QA `SendKey` handler (`app/executor.rs`) both call
`input::browse_key_to_command` / `input::browse_qa_key_to_command` when `browser.is_browse()`, else the image-mode
mappings (image mode is byte-for-byte unchanged). Browse keys: Esc/Enter → `EnterImageMode`; Tab → `ToggleBrowseFocus`;
Up/Down → `BrowseMoveTreeSelection(∓1)`; Right/Left → `BrowseExpandTreeSelection(true/false)`. The `main.rs`/executor
Esc special-case (fullscreen-or-quit, `AppCommand::Exit`) is never reached in browse mode because Esc maps to
`EnterImageMode` before it, so Esc returns to image mode and never quits.

**Focused pane is app-tracked,** not the native key-view loop: `browser::State::focused_pane`
(`PaneSide::{Tree, Grid}`). `ToggleBrowseFocus` flips it and calls `split_view::set_focused_pane`, which recolors the
panes so the focused one is highlighted. The native views will render their own selection regardless of first-responder
state, so this app-managed focus is enough. `SharedAppState` exposes `view_mode`, `focused_pane`, and
`browse_selected_folder` (also at `GET /state`) so QA/tests can assert the mode swap, focus flip, and tree selection
without real keystrokes.

## Gotchas

- **`Retained<>` must outlive the window.** `BrowseSplitView` stores the split view, both panes, and the `BrowseTree`
  (which owns the outline view + the `TreeDataSource`, since `setDataSource:`/`setDelegate:` are weak `assign`).
  Dropping early segfaults the autorelease pool (no compile-time check) — see `platform/macos/CLAUDE.md`.
- **Never pass `&Retained<T>` to the `as_view`/`as_nsview` pointer cast — deref first (`&*x`).** Those helpers do a raw
  `*const T as *const NSView` cast; handed `&Retained<NSTextField>` they reinterpret the smart-pointer struct's memory
  as an NSView, so AppKit messages a garbage isa and aborts with `objc[...]: Attempt to use unknown class 0x…`. (Method
  args typed `&NSView`/`&AnyObject` are fine — Rust deref-coerces a `&Retained<…>` to the real object there. The cast
  helpers are the trap because the generic `&T` swallows the `Retained`.) Bit us building the tree cell.
- **`NSSplitView` sizes its arranged subviews itself.** The panes KEEP `translatesAutoresizingMaskIntoConstraints` ON
  (the default). Disabling it on the arranged subviews (and giving them no size constraints) collapses both panes to
  zero — the gray void the first spike rendered. Set an initial divider position with `setPosition:ofDividerAtIndex:` so
  neither pane starts collapsed.
- **Build on the main thread.** `BrowseSplitView::create` asserts the `MainThreadMarker`; it's only ever called from the
  winit event loop (main thread).
