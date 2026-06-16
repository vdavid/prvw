# Browser (browse mode)

A second top-level screen for the main window: a native AppKit folder tree + thumbnail grid that **swaps** with the wgpu
image viewer. Both panes are real now: the **tree** (Phase 3) is an `NSOutlineView` source list of the home folder +
mounted volumes; the **grid** (Phase 4) is an `NSCollectionView` thumbnail gallery of the selected folder's images,
driven by the Phase-2 scheduler + cache plumbing. Full design: `docs/specs/image-browser.md`.

| File                 | Purpose                                                                                                                                                                                      |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `mod.rs`             | `ViewMode` + `PaneSide` enums; `browser::State` (mode, `focused_pane: Option<PaneSide>` single source of truth, selected folder, grid selection, sort, native handles); pure `next_focused_pane`/`browse_entry_pane`/`browse_keydown_command`; `apply_focus`; tree + grid delegation; tests |
| `split_view.rs`      | macOS `NSSplitView` build, hide/show, `apply_focus` (syncs native first responder + grid emphasis to `focused_pane`), divider + traffic-light fixes; hosts the tree (left) and the grid (right) |
| `grid.rs`            | macOS `NSCollectionView` grid: `BrowseCollectionView` (keyDown override), `GridItem` (cell, focus-aware selection rect + double-click in `mouseDown:`), `GridDataSource` (data source + delegate + prefetch, owns the grid's mutable state), `BrowseGrid` (owns the views + drives listing/thumbs/focus) |
| `grid_model.rs`      | Pure, headless-tested: the folder image list + sort + selected index + empty detection + folder generation, and `clamp_visible_range`                                                        |
| `grid_listing.rs`    | Background folder-image lister (its own OS thread + `mpsc`, like the tree scanner) + the pure `list_supported_images`                                                                        |
| `grid_scheduler.rs`  | Pure, headless-tested: visible-range-centered generation order for the grid (the grid's `BrowseGrid::pump` drives it)                                                                        |
| `thumbnail_cache.rs` | Pure, headless-tested: 128 MB byte-budget, distance-from-visible-range eviction state + the `MAX_CELL_PT`/`GRID_THUMBNAIL_PX` size constants                                                 |
| `outline.rs`         | macOS `NSOutlineView` source-list tree: `BrowseOutlineView` (keyDown override), `NodeObject`, `TreeDataSource` (data source + delegate), `BrowseTree` (owns the view + `make_first_responder`) |
| `tree_model.rs`      | Pure, headless-tested logic: `child_directories`, `enumerate_roots`/`build_roots`, the `ChildCache` load-state machine, and the `scan_overdue` overlay predicate                              |

## The swap

`App` holds `browser: browser::State`. `App::set_view_mode` (in `app.rs`) drives it:

- **Image → Browse:** build the split view on first use, unhide it, `window::set_metal_layer_hidden(true)`, focus the
  tree pane. No redraw requested — the GPU goes idle (render-on-demand).
- **Browse → Image:** hide the split view, `set_metal_layer_hidden(false)`, `request_redraw()`.

The split view is a **sibling subview of winit's contentView** at `zPosition` 2.0 (above the Metal layer's 1.0), pinned
to all four edges, identifier `prvw.browser_split`, hidden at startup. Same pattern as `window::add_titlebar_labels`. A
transparent Metal pixel occludes content behind it, so the native UI must sit in front, not behind — hence
hide-one-show-the-other, not compositing.

Commands: `ToggleBrowseMode` (menu + Enter in image mode), `EnterImageMode` (Esc while browsing; Enter on the tree),
`ToggleBrowseFocus` (Tab while browsing), `BrowseSelectFolder(PathBuf)` (tree selection — also focuses the tree pane),
`BrowseFolderListed { folder, images }` (a background listing finished), `BrowseThumbnailsAvailable` (grid-thumbnail
completions queued), `BrowseGridSelected(usize)` (grid click/selection — also focuses the grid pane), `BrowseOpenSelected`
(Enter on the focused grid, or a double-click — opens the selected image, else falls back to image mode). Browse keys are
intercepted by the focused native view's `keyDown:` override (`browser::browse_keydown_command`), not winit. Dispatched
in `app/executor.rs`. All but `ToggleBrowseMode`/`EnterImageMode`/`ToggleBrowseFocus`/`BrowseOpenSelected` are
macOS-only.

## The folder tree (Phase 3)

`outline.rs` builds the left pane: an `NSOutlineView` with `setStyle(NSTableViewStyle::SourceList)` (the rounded-pill
source-list selection + inset; `setSelectionHighlightStyle:` is deprecated) inside an `NSVisualEffectView` `.sidebar`
material. One column, directories only, children loaded asynchronously (see below — the data source never reads a
directory on the main thread).

- **Item identity is load-bearing.** `NSOutlineView` tracks items by pointer identity — it hands the data-source methods
  back the exact pointers they returned earlier and compares by address. So each node is a `NodeObject` (a tiny
  `NSObject` subclass holding the node's `PathBuf`), and `TreeDataSource::node_for_path` returns the **same**
  `Retained<NodeObject>` per path from a `RefCell<HashMap<PathBuf, Retained<NodeObject>>>` cache. Return a fresh object
  per call and the tree misbehaves (wrong expansion, lost selection).
- **Roots are flat, not grouped.** Home folder first, then each mounted volume as a sibling top-level row. A
  Finder-style "Locations" group header needs group-row pseudo-items (no path), which fights the path-keyed node model —
  so flat roots. Volumes come from `NSFileManager mountedVolumeURLsIncludingResourceValuesForKeys:options:`
  (skip-hidden), with each volume's localized name via `NSURLVolumeLocalizedNameKey`; falls back to listing `/Volumes`.
  Row order/labels are decided by the pure `tree_model::build_roots` (unit-tested); `enumerate_roots` is the macOS glue
  that feeds it.
- **Arrow keys are native.** The tree pane's first responder is the `BrowseOutlineView` (synced by `apply_focus`), so
  Up/Down/Left/Right (and type-select) are handled by `NSOutlineView` itself — no programmatic selection code. The
  subclass's `keyDown:` only intercepts Tab/Enter/Esc and calls `super` for the rest.
- **Selection → app state → grid listing.** `outlineViewSelectionDidChange:` sends `BrowseSelectFolder(path)`; the
  executor stores it in `State::selected_folder` and kicks off a background listing of that folder's images for the grid
  (`grid_listing::FolderLister`). It never reads the directory on the main thread — a slow folder selection must not
  freeze the UI (mirrors the tree's async fix).

### Children load asynchronously — never read a directory on the main thread

The data source's child enumeration is **fully async**. Reading a directory inline on the main thread freezes winit's
event loop (the whole app) whenever the filesystem is slow — a stale SMB mount blocks for ~10 s. So:

- The data source serves children **only from an in-memory cache** (`tree_model::ChildCache`, a
  `NotLoaded → InFlight → Loaded` state machine). It never calls `read_dir` itself.
- On a cache miss (`numberOfChildrenOfItem:` for a not-yet-scanned path), `loaded_or_request` marks the path `InFlight`,
  enqueues a scan on the background `TreeScanner` thread (`std::thread` + `mpsc`, mirroring `navigation::preloader` — no
  tokio), and reports **0 children** for now. `begin_scan` returns `false` for an already-in-flight/loaded path, so the
  same dir is scanned once no matter how often the outline view re-queries during layout.
- The scanner reads the directory off-thread (`tree_model::child_directories`) and posts the result back to the main
  thread via `crate::commands::send_command(AppCommand::BrowseTreeChildrenLoaded { path, children })` (the global
  `EventLoopProxy`). The executor stores it (`complete_scan`) and calls `reloadItem:reloadChildren:` for that node, so
  the outline view re-queries and shows the rows.
- **`isItemExpandable:` and `child:ofItem:` never read the disk.** `isItemExpandable:` assumes any directory is
  expandable (returns `YES` on a miss/in-flight, so the disclosure triangle shows; a folder that scans empty loses its
  triangle on the reload). `child:ofItem:` serves only from the loaded cache (it's called only for indices
  `numberOfChildren` already reported, i.e. post-load).
- **Roots' children are async too** (the SMB volume's contents are the slow case): they go through the exact same path
  when AppKit first asks for a root's child count. The roots **list** (home + `mountedVolumeURLs`) stays synchronous —
  it's a fast metadata call, not a directory walk.

**Loading overlay (tree pane only).** A scan the user is waiting on that outlives `tree_model::LOADING_OVERLAY_DELAY` (1
s) reveals a translucent (~0.8) "Loading…" overlay over the tree pane (`BrowseSplitView::loading_overlay`), hidden again
when scans finish. The 1 s delay keeps it from flashing for fast local dirs. It's driven by the existing wakeup
mechanism: `ChildCache::earliest_in_flight` feeds `App::schedule_wakeup` (which schedules a wakeup at the 1 s deadline)
and `App::about_to_wait` calls `refresh_loading_overlay` each pass, which consults the pure `tree_model::scan_overdue`
predicate. While a scan is in flight the rest of the UI (Tab, Esc/Enter mode switching) stays fully responsive — the
freeze is contained to the tree pane's loading state, because the main thread never blocks on the read.

### First responder restored on entering image mode

Browse mode hosts a live `NSOutlineView` that holds (or some descendant holds) the window's first responder. On Esc →
image the hidden outline view can keep the responder, so winit never receives the next key and image-mode Enter does
nothing (the menu still works — muda events bypass the responder chain). `browser::State::enter_image` therefore calls
`window::restore_content_view_first_responder` (`makeFirstResponder:` the winit `ns_view`), handing the keyboard back to
winit so the Enter → browse → Esc → image → Enter cycle repeats indefinitely. **Don't drop this call** — without it,
Enter→browse only works once per session.

### Two fixes baked into `split_view.rs`

- **Divider opens at ~240pt without a drag.** `setPosition:ofDividerAtIndex:` no-ops at build time (zero frame). We set
  it once in `set_hidden(false)` after `layoutSubtreeIfNeeded` forces the edge constraints to resolve, latched by a
  `Cell<bool>` ivar so a later show won't yank a divider the user dragged. (An earlier attempt set it from an overridden
  `layout` — `setPosition:` re-enters `layout` synchronously, so it must not run there.)
- **Sidebar clears the traffic lights.** The `.sidebar` vibrancy fills the pane, but the outline scroll view is inset
  `crate::TITLE_BAR_HEIGHT` (32pt) from the top so no row sits under the traffic-light strip.

## Focus / keyboard

**Why the native responder chain works here.** In browse mode the GPU layer is hidden and the app stops requesting
redraws, so winit is idle and does NOT re-assert first responder. The focused native view holds the window's first
responder and handles its own keys. (The Phase 0 spike's first read — winit winning first responder — was a stub
artifact: plain placeholder panes with redraws still firing. Verified in idle-winit browse mode: `makeFirstResponder`
accepted, no winit re-assertion.)

**Single source of truth:** `browser::State::focused_pane: Option<PaneSide>` — `None` in image mode,
`Some(Tree)`/`Some(Grid)` in browse mode. Nothing else decides which pane is focused; it's never inferred from the native
first responder. `apply_focus` is the sync point: it `makeFirstResponder:`s the focused pane's control (the
`BrowseOutlineView` for Tree, the `BrowseCollectionView` for Grid) and refreshes the grid's emphasis. Called on every
focus change.

**Focus transitions.** `enter_browse` focuses the grid if the selected folder has images, else the tree
(`browse_entry_pane`). `enter_image` sets `focused_pane = None` and restores the winit content view as first responder.
Tab flips Tree↔Grid, skipping an empty grid (`next_focused_pane`) — an empty grid is non-focusable, so Tab stays on the
tree. A grid click focuses Grid (`set_grid_selected`); a tree selection focuses Tree (`set_tree_focused`, from the
`BrowseSelectFolder` executor arm). `next_focused_pane` / `browse_entry_pane` are pure and headless-tested.

**Keys via the focused view's `keyDown:` override, not winit.** `BrowseOutlineView` and `BrowseCollectionView` subclass
their controls and override `keyDown:` to intercept only Tab → `ToggleBrowseFocus`, Enter (Return/keypad-Enter) →
`BrowseOpenSelected` (opens the selected image when the grid is focused, else falls back to image mode — so Enter on the
tree returns to the viewer), Esc → `EnterImageMode`. Everything else (arrows, page keys, type-select) calls `super`, so
native selection/scroll stays immediate. The map is the pure `browser::browse_keydown_command(key_code)`, routed via
`crate::commands::send_command`. A defensive `input::browse_key_to_command` still maps Tab/Enter/Esc in case winit ever
delivers a key in browse mode, but with first responder held by the native view it normally doesn't fire. **No
winit-routed browse arrow handling exists** (arrows are native).

**Emphasis follows focus.** Tree: a source-list `NSOutlineView` draws accent-blue selection when it's first responder and
gray otherwise — so syncing first responder to `focused_pane` makes it correct for free. Grid: `GridItem::setSelected:`
and `BrowseGrid::refresh_focus_emphasis` (called by `apply_focus` on a Tab flip) draw a rounded rect — accent-blue when
selected and the grid is first responder, gray when selected but not, nothing when not selected. The item reads its own
first-responder state (`grid_is_first_responder`) at paint time.

`SharedAppState` exposes `view_mode`, `focused_pane` (`"tree"`/`"grid"`/`"none"`), `browse_selected_folder`, and
`browse_grid_selected` (also at `GET /state`) so QA/tests can assert the mode swap, focus flip, tree selection, and grid
selection without real keystrokes. The QA `SendKey` path maps only Tab/Enter/Esc in browse mode (arrows are native, so
the QA path can't drive native selection by key).

## The thumbnail grid (Phase 4)

`grid.rs` builds the right pane: an `NSCollectionView` (`NSCollectionViewFlowLayout`, vertical scroll) of ~160pt square
cells, each a `GridItem` (an `NSCollectionViewItem` subclass with a proportionally-scaling `NSImageView` + a filename
label). The slider that live-resizes cells is a later phase — Phase 4 uses a fixed cell size.

- **Where the mutable state lives.** `NSCollectionView` holds its data source/delegate weakly, so `BrowseGrid` keeps the
  `Retained<GridDataSource>` alive for the window's life. All the grid's mutable state — the `grid_model::GridModel`,
  the `grid_scheduler::Scheduler`, the `thumbnail_cache::ThumbnailCache`, the generated-`NSImage` map, the
  `grid_listing::FolderLister`, and the grid's `quicklook::RequestTable` — lives in `RefCell` ivars on `GridDataSource`
  (main-thread only; AppKit calls re-entrantly). `BrowseGrid`'s methods delegate into the data source.
- **Raw-selector data source.** `GridDataSource` implements `numberOfItemsInSection:`,
  `itemForRepresentedObjectAtIndexPath:`, `didSelectItemsAtIndexPaths:`, and `prefetchItemsAtIndexPaths:` as raw
  `#[unsafe(method(...))]` arms (like `outline::TreeDataSource`), **not** the typed protocol traits — a `define_class!`
  method can't return `Retained<T>` (no `Encode`) the way `NSCollectionViewDataSource` demands. It's registered via raw
  `setDataSource:`/`setDelegate:`/`setPrefetchDataSource:` passing the object as `&AnyObject`; AppKit only sends the
  selectors, it doesn't check Rust conformance.
- **Async folder listing.** Selecting a folder in the tree starts a background listing on `grid_listing::FolderLister`
  (its own OS thread + `mpsc`, like the tree scanner — newest request wins). The worker reads the dir off-thread and
  posts `BrowseFolderListed { folder, images }`; the executor calls `BrowseGrid::folder_listed`, which sorts via the
  model's `SortBy`, reloads the collection view, toggles the "(No images)" overlay, and seeds the scheduler/cache. This
  subsumes the old main-thread `count_supported_images` read — the listing returns the actual paths off-thread.
- **Open → image mode.** Double-click (detected in `GridItem::mouseDown:` via `clickCount == 2` — not a click gesture
  recognizer, which delays the single click ~600 ms) or Enter on the focused grid fires `BrowseOpenSelected`. Single
  click selects instantly. `App::open_selected_grid_image` builds a `DirectoryList::from_explicit` from
  the grid's image list (same `SortBy`, so the order matches), positions it at the selected index via `go_by`, switches
  to image mode, seeds previews, displays the image, and warms neighbors (`warm_initial_neighbors`, shared with launch)
  — so arrow-key nav works immediately. Enter on the tree (or with no grid selection) just returns to image mode.

### Thumbnails: same QL worker as previews, AppKit owns the bitmaps

Grid thumbnails ride a **second `quicklook::RequestTable`** — a second request path into Finder's shared `quicklookd`
cache (the `QLThumbnailGenerator` singleton), not a second engine. `RequestTable::new` takes the wake `AppCommand`
constructor (`|| BrowseThumbnailsAvailable`) and a worker-thread name (`prvw-gridgen`); everything else is shared with
previews. The worker delivers RGBA8 (`cg_image_to_rgba8`); the grid's consumption seam is
`quicklook::nsimage_from_rgba8` (main-thread RGBA8 → `NSImage` via `NSBitmapImageRep`, because `NSImage` isn't `Send`).

- **Scroll → scheduler/cache.** `App::about_to_wait` calls `BrowseGrid::pump_visible_range` while browsing: it reads the
  collection view's visible item range (widened by `PREFETCH_MARGIN`), feeds it to `Scheduler::set_visible_range` +
  `ThumbnailCache::set_visible_range`, and pumps `Scheduler::poll_next` into the QL worker at `GRID_THUMBNAIL_PX`.
  Native scrolling routes through the same run loop, so `about_to_wait` fires after a scroll; the scheduler dedups, so
  the pump is cheap when nothing changed. `NSCollectionViewPrefetching` widens the range further ahead/behind.
- **Completion → cell.** `BrowseThumbnailsAvailable` → `BrowseGrid::thumbnails_available`: drops stale-generation
  deliveries (the model's folder generation guards them), wraps RGBA8 in an `NSImage`, stores it in the map + the
  cache's byte bookkeeping, then **feeds `evict_to_budget()`'s returned indices to `Scheduler::uncache`** (Phase 2's
  invariant) and drops their `NSImage`s, and reloads the affected items so cells pick up their image.
- **Pure plumbing.** `grid_scheduler::Scheduler` (visible-range-centered order, `MARGIN` = 100 cap) and
  `thumbnail_cache::ThumbnailCache` (128 MB byte budget, farthest-from-range eviction, ties by least-recently-touched)
  stay pure and unit-tested; `grid.rs` is their runtime caller. Both share `grid_scheduler::distance_from_range` so
  scheduling and eviction agree.

**Size constant:** `thumbnail_cache::MAX_CELL_PT` (256pt) → `GRID_THUMBNAIL_PX` (512px = 256 × 2 Retina). One max-size
RGBA8 thumbnail is `512 × 512 × 4 ≈ 1 MB` (`EST_THUMBNAIL_BYTES`), so 128 MB ≈ 128 resident thumbnails. Generated
**once** at this size; smaller cells downscale the cached bitmap — never regenerated on resize.

## Gotchas

- **`Retained<>` must outlive the window.** `BrowseSplitView` stores the split view, both panes, the `BrowseTree`, and
  the `BrowseGrid` (each owns its view + its `TreeDataSource`/`GridDataSource`, since `setDataSource:`/`setDelegate:`/
  `setPrefetchDataSource:` are weak `assign`). Dropping early segfaults the autorelease pool (no compile-time check) —
  see `platform/macos/CLAUDE.md`.
- **Register the grid item class via `GridItem::class()`, not a name lookup.**
  `objc2::runtime::AnyClass::get(c"PrvwGridItem")` returns `None` until the `define_class!` type is first referenced
  (registration is lazy), so it panics at grid-build time. `GridItem::class()` forces registration and hands back the
  class. Bit us on first browse entry.
- **A `define_class!` method can't return `Retained<T>`** (no `Encode` impl) and `#[unsafe(method_family = ...)]` isn't
  supported alongside `#[unsafe(method(...))]` there. So `GridDataSource` implements the collection-view protocol
  selectors as raw methods returning `*mut NSCollectionViewItem` (via `Retained::into_raw`) rather than conforming to
  the typed `NSCollectionViewDataSource` trait, and gets wired with raw `setDataSource:` sends.
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
