# Browser (browse mode)

A second top-level screen for the main window: a native folder tree + thumbnail grid that **swaps** with the wgpu image
viewer, driven by the visible-range scheduler + byte-budget cache. Full design: `docs/specs/image-browser.md`.

**Two native shells, one set of models.** This directory is the macOS one — an `NSOutlineView` source list of the home
folder + mounted volumes, and an `NSCollectionView` thumbnail gallery. The Windows one is
[`windows/`](windows/CLAUDE.md): a `SysTreeView32` and a virtual `SysListView32`, led by known folders and drive letters
rather than a home folder, with a splitter and a status bar of its own. Neither is a port of the other
(`docs/specs/windows-ui-design.md` → "Browse mode"), and the seam between them is that everything **below** the widgets
is shared and platform-free: `grid_model`, `grid_scheduler`, `thumbnail_cache`, `tree_model` (`TreeScanner` included),
and `grid_listing`.

`browser::State` carries both. Its pure transitions (`enter_browse_state`, `enter_image_state`, `toggle_focus_state`,
`focus_grid_state`) are shared and tested; each platform's windowed methods wrap them with its own `sync_native`. A gate
that reads `#[cfg(any(target_os = "macos", target_os = "windows"))]` anywhere in the app means "where browse mode has a
UI"; Linux has neither and falls back to `set_view_mode(Image)`.

| File                 | Purpose                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- |
| `mod.rs`             | `ViewMode` + `PaneSide` + `LaunchTarget` enums; `browser::State` (mode, `focused_pane: Option<PaneSide>` single source of truth, selected folder, grid selection, sort, `pending_grid_preselect` + `pending_browse_open_focus` for browse-open, native handles); the `sync_native` render-from-state choke-point; `reveal_to_folder`; QA accessors (`grid_count`, `reveal_pending`) + the `qa_select_grid_index` test-driving hook; pure `next_focused_pane`/`browse_entry_pane`/`browse_keydown_command`/`grid_preselect_index`/`classify_launch_target` + field-transition cores; tree + grid delegation; tests |
| `split_view.rs`      | macOS `NSSplitView` build, hide/show, `apply_focus` (makes the focused pane's control first responder + refreshes grid emphasis — called by `sync_native`), divider + traffic-light fixes; hosts the tree (left) and the grid (right)                                                                                                                                                                                                                                                                                                                                                                             |
| `grid.rs`            | macOS `NSCollectionView` grid: `BrowseCollectionView` (keyDown override), `GridItem` (cell, focus-aware selection rect + double-click in `mouseDown:`), `GridDataSource` (data source + delegate + prefetch, owns the grid's mutable state), `BrowseGrid` (owns the views + drives listing/thumbs/focus)                                                                                                                                                                                                                                                                                                          |
| `grid_model.rs`      | Pure, headless-tested: the folder image list + sort + selected index + empty detection + folder generation, and `clamp_visible_range`                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `grid_listing.rs`    | Background folder-image lister (its own OS thread + `mpsc`, like the tree scanner) + the pure `list_supported_images`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `grid_scheduler.rs`  | Pure, headless-tested: visible-range-centered generation order for the grid (the grid's `BrowseGrid::pump` drives it)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `thumbnail_cache.rs` | Pure, headless-tested: 128 MB byte-budget, distance-from-visible-range eviction state + the `MAX_CELL_PT`/`GRID_THUMBNAIL_PX` size constants                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `outline.rs`         | macOS `NSOutlineView` source-list tree: `BrowseOutlineView` (keyDown override), `NodeObject`, `TreeDataSource` (data source + delegate), `BrowseTree` (owns the view + `make_first_responder` + the async `reveal_to_folder` walk)                                                                                                                                                                                                                                                                                                                                                                                |
| `tree_model.rs`      | Pure, headless-tested logic: `child_directories`, `enumerate_roots`/`build_roots`, `reveal_path_chain` (root-to-target reveal walk), the `ChildCache` load-state machine, the `scan_overdue` overlay predicate, and the shared `TreeScanner` thread                                                                                                                                                                                                                                                                                                                                                               |
| `windows/`           | The Windows shell: the tree, the grid, the splitter, and the status bar. Its own [`CLAUDE.md`](windows/CLAUDE.md)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |     |

## The swap

`App` holds `browser: browser::State`. `App::set_view_mode` (in `app.rs`) drives it via `enter_browse` / `enter_image`,
which set state then render through `sync_native` (see "Browse UI architecture" below — `sync_native` derives split-view
and Metal-layer visibility, the image labels, first responder, and emphasis from `mode` + `focused_pane`):

- **Image → Browse:** clear the bound image and paint ONE black frame while the Metal layer is still VISIBLE (so its
  last-composited frame is black — see "Black-not-stale reveal" below), build the split view on first use, grow the
  window to the browse minimum if it's smaller (`window::grow_to_browse_minimum`, ~860×560 — a small image's
  fit-to-window may have shrunk it), set `mode = Browse` + focus (grid if it has images, else tree), `sync_native`
  (which hides the Metal layer). No further redraw requested — the GPU goes idle (render-on-demand). The min-size is
  enforced on browse entry only, never in image mode (so it doesn't fight fit-to-window).
- **Browse → Image:** the user-facing exits (Esc / Enter / double-click / the menu) go through
  `App::reveal_selected_image`, NOT `set_view_mode(Image)` — see "Esc == Enter == reveal" below. Reveal is
  **black-not-stale**: `browser::State::reveal_image_canvas` sets `mode = Image` + `focused_pane = None`, hides the
  split view, restores winit's first responder, and UNHIDES the Metal layer; the app then synchronously paints the
  target. The worst the user can see is the black-left-behind frame briefly, never the stale previous image (see
  "Black-not-stale reveal"). (`enter_image` via the plain `set_view_mode(Image)` path — which unhides immediately
  through `sync_native` — survives for the non-macOS build and as the underlying mode-setter; the macOS browse-exit
  always reveals.)

The split view is a **sibling subview of winit's contentView** at `zPosition` 2.0 (above the Metal layer's 1.0), pinned
to all four edges, identifier `prvw.browser_split`, hidden at startup. Same pattern as `window::add_titlebar_labels`. A
transparent Metal pixel occludes content behind it, so the native UI must sit in front, not behind — hence
hide-one-show-the-other, not compositing.

Commands: `ToggleBrowseMode` (menu + Enter in image mode), `EnterImageMode` (Esc while browsing; Enter on the tree),
`ToggleBrowseFocus` (Tab while browsing), `BrowseSelectFolder(PathBuf)` (tree selection — also focuses the tree pane),
`BrowseFolderListed { folder, images }` (a background listing finished), `BrowseThumbnailsAvailable` (grid-thumbnail
completions queued), `BrowseGridSelected(usize)` (grid click/selection — also focuses the grid pane),
`BrowseOpenSelected` (Enter on the focused grid, or a double-click). Browse keys are intercepted by the focused native
view's `keyDown:` override (`browser::browse_keydown_command`), not winit. Dispatched in `app/executor.rs`. All but
`ToggleBrowseMode`/`EnterImageMode`/`ToggleBrowseFocus`/`BrowseOpenSelected` are macOS-only.

## Browse-open positioning + dir-arg launch

**Every browse entry re-anchors to the live current image.** On **each** entry into browse from an image (Enter or
Navigate → Image browser), `set_view_mode(Browse)` runs `enter_browse` then `App::reveal_current_image_in_browse`: it
reveals + selects the current image's folder in the tree and preselects that image in the grid, scrolling both into
view. The anchor target (the image's parent folder + the image) is computed by the pure `browser::browse_anchor_target`.
This runs on every entry, not just the first — so after navigating in image mode (arrow keys) and pressing Enter, browse
shows the image you're _currently_ viewing, never the stale selection from the last time you browsed
(`docs/specs/live-folder-sync.md` Part 1). Esc/Enter right after opening round-trips to the same image. Empty / no-image
cases fall back gracefully to the last browse state (first image, or the tree for an empty folder).

**One entry names its own folder instead: a dropped one.** `App::browse_folder` (what a folder drop calls) passes the
folder through `enter_view_mode`'s `reveal` argument, which replaces the "reveal where you already are" step rather than
following it. Queueing both would put two tree walks in flight for one entry, and whichever landed last would decide the
selection. Already in browse mode, the same call just re-reveals, so dropping a folder onto the browser moves the tree.

**Re-reveal into the already-selected folder.** When the current image's folder is already the one selected in the tree
(re-entering browse without changing folders), `select_and_scroll_to`'s `selectRowIndexes:` is a no-op, so
`outlineViewSelectionDidChange:` never fires and the grid would keep its stale selection. So `select_and_scroll_to`
detects the already-selected row and dispatches `BrowseSelectFolder` directly, forcing a re-list that consumes the
pending preselect and re-anchors the grid to the current image (scrolling it into view). This is what makes the
re-anchor hold even when only the _image within the folder_ changed.

**The async reveal-path walk** (`outline::BrowseTree`). Child directories load on the background scanner, so "reveal a
path" can't expand synchronously — there'd be no children yet. Instead it's a pending walk advanced by
`BrowseTreeChildrenLoaded`:

- `tree_model::reveal_path_chain(roots, target)` (pure, tested) computes the root-to-target path list: the **root** is
  the longest-prefix `Root` match (a path under home reveals under Home, not the `/` volume), then every intermediate
  directory, ending at `target`. `None` when no root contains `target`. The chain's first element is the root's own
  spelling, never the target's, because the caller looks rows up by path.

  **Every step of it goes through a `PathPolicy`**, the walk included: `reveal_path_chain_under(policy, …)` is the real
  function and `reveal_path_chain` is the host wrapper. So a canonical `\\?\C:\Users\dave\pics` reveals under the `C:\`
  row a drive enumeration produced, `\\?\UNC\naspi\photos\...` reveals under a `\\naspi\photos` root, and
  `c:\users\dave` finds a `C:\Users\Dave` row — all of it asserted **from a Mac**, because the policy is an argument
  rather than a `cfg`. `Path::ancestors` and `Path::components` both split on the host's separators, which is why
  `PathPolicy::ancestors` and `PathPolicy::component_count` exist.

- `BrowseTree::reveal_to_folder(folder)` computes the chain from its own roots and starts a
  `RevealWalk { chain, position }`. It expands `chain[0]` (the root), which makes AppKit query its children → enqueues
  the scan (no main- thread disk read). `children_loaded` calls `advance_reveal`: when the level it's waiting on
  (`chain[position]`) arrives, it steps `position` forward and expands the next ancestor; at the target it selects +
  scrolls-to-mid and clears the walk. A level whose children are **already cached** (a re-reveal) advances synchronously
  so the walk doesn't stall waiting for a scan that won't re-fire. A single-element chain (target is a root) selects
  immediately.
- The terminal `select_and_scroll_to` fires `outlineViewSelectionDidChange:` → `BrowseSelectFolder`, so the reveal
  drives the grid listing through the normal path. `scroll_row_to_middle` centers the row in the scroll clip (vs the
  default "just make visible").

**Grid preselect + grid focus on browse-open.** `browser::State` holds a one-shot
`pending_grid_preselect: Option<PathBuf>` (the came-from image) and `pending_browse_open_focus: bool`, both set by
`State::reveal_to_folder` and consumed by `grid_folder_listed` — but **only for a listing of the folder the reveal is
walking towards**, which `pending_reveal_folder` remembers and `reveal_landing_matches` (pure/tested) decides. A listing
for any other folder can land first: on Windows a treeview selects its own first row the moment it takes focus, so the
drive root listed before the walk had gone anywhere, and spending the one-shot state there left the grid preselecting
image 0 with the focus still on the tree. When the revealed folder's images land,
`BrowseGrid::folder_listed(images, preselect)` selects the preselect image's index (`browser::grid_preselect_index`,
pure/tested — maps the came-from path to its slot in the SORTED list) and scrolls it into view, else index 0; and
`grid_folder_listed` moves focus to the grid (the reveal's tree selection had focused the tree, since the grid was empty
when `enter_browse` ran). An empty revealed folder keeps the tree focused (the grid is non-focusable).

**Dir-arg launch** (`main.rs` + `app.rs`). `browser::classify_launch_target(is_file, is_dir)` (pure/tested) maps a
single CLI argument: a file → image mode (unchanged), a directory → browse mode, neither → onboarding (like no
argument). `main.rs` detects a lone directory arg, canonicalizes it, and passes `App::new(..., launch_directory, ...)`.
`initialize_viewer` then skips the initial-image display (no `dir_list`, the user opens one from the grid) and, after
the window/renderer/menu/preloader are up, runs `enter_browse` + `reveal_to_folder(dir, None)` (no came-from image →
grid preselects the first image, or the tree for an empty folder). Multiple args stay an image set; a missing/unreadable
path falls through to onboarding.

**Arrow-key pane isolation** is automatic from the focus model: exactly one pane is first responder (synced by
`sync_native`/`apply_focus`), arrows fall through to `super` (native) only on that view, and the winit/QA browse key
maps (`input::browse_key_to_command` / `browse_qa_key_to_command`) route only Tab/Enter/Esc — never arrows. So arrows
move only the focused pane; the other never moves. No extra code.

**Esc == Enter == reveal the selected image.** All three browse-exit commands (`EnterImageMode`, `BrowseOpenSelected`,
and the Browse→image direction of `ToggleBrowseMode`) route to one place: `App::reveal_selected_image`. There's no "Esc
preserves the previously displayed image" path — the user's model is that the image-mode current image IS whatever the
browse cursor points at. Reveal points `navigation` at the grid's folder + selected index (`resolve_reveal_index` maps
the grid's index 1:1 to the dir-list index, since both use the same `SortBy`), then displays it. With no grid selection
(tree focused, or empty folder) it degrades gracefully — reveals image mode still showing the last valid image, never a
blank/stale flash.

**Black-not-stale reveal (never the previous image).** The reveal never shows the old image. Two facts force the design:

1. **Presenting to a hidden `CAMetalLayer` doesn't commit.** Painting a frame while the layer is `hidden` then unhiding
   it does NOT update what's shown — the layer keeps its last-VISIBLE composited frame, and the new frame only lands
   ~100 ms later on the next compositor pass. So "paint-while-hidden then unhide" can't eliminate the stale frame.
2. **A new image's geometry must never composite the old texture.** On a cache miss with no usable placeholder, the new
   image's auto-fit sets a new transform; with the previous image's texture still bound, the renderer would stretch that
   texture to the new geometry — the distorted stale-frame look.

The fix has three parts:

- **Make the last-VISIBLE frame black on browse entry.** `App::set_view_mode(Browse)` calls `renderer.clear_image()`
  (drops the bound texture + destroys its backing) and paints ONE `render_frame()` while the Metal layer is still
  visible, THEN `enter_browse` hides it. The split view covers the canvas immediately, so this black frame isn't seen on
  the image→browse transition — but it's now the layer's last-composited content.
- **The renderer never composites a stale texture.** When no image is bound, `Renderer::render` fills the image area
  (below the title-bar strip) with OPAQUE black via the overlay pipeline instead of leaving the transparent clear (which
  would show the compositor's last frame bleeding through). The title-bar strip stays transparent so its vibrancy still
  shows. `display_open_target`'s cache-miss-with-no-placeholder branch also calls `renderer.clear_image()` after the
  auto-fit, so the new geometry shows clean black, never the previous image stretched.
- **Reveal unhides first, then paints.** `browser::State::reveal_image_canvas` sets image-mode state + hides the split
  view + restores winit's first responder + UNHIDES the Metal layer; the app then displays the target
  (`display_open_target`) and renders one frame synchronously (`App::render_frame`) — all in the same event-loop
  callback. Because the last-visible frame was black, the worst the user sees is a brief black → correct image, never
  the stale stretched previous image. (We deliberately do NOT paint the selection while still browsing — that would
  auto-fit-resize the window behind the browse UI. The selection is warmed into the cache instead, see below, so the
  reveal paint is usually a cache hit and lands the correct image in that one frame.)

## The folder tree

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
  `NotLoaded → InFlight → Loaded` state machine). It never calls `read_dir` itself. The cache is keyed through
  `paths::PathPolicy::key`, not on the `PathBuf`: a scan is requested under the tree row's spelling of a folder while a
  reveal walk asks about the canonicalized one, and a byte-keyed map answers "not loaded" to a folder it has.
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
nothing (the menu still works — muda events bypass the responder chain). `sync_native` therefore calls
`window::restore_content_view_first_responder` (`makeFirstResponder:` the winit `ns_view`) in image mode, handing the
keyboard back to winit so the Enter → browse → Esc → image → Enter cycle repeats indefinitely. **Don't drop this** —
without it, Enter→browse only works once per session. (Detail in "Browse UI architecture" below.)

### Two fixes baked into `split_view.rs`

- **Divider opens at ~240pt without a drag.** `setPosition:ofDividerAtIndex:` no-ops at build time (zero frame). We set
  it once in `set_hidden(false)` after `layoutSubtreeIfNeeded` forces the edge constraints to resolve, latched by a
  `Cell<bool>` ivar so a later show won't yank a divider the user dragged. **Don't** set it from an overridden `layout`:
  `setPosition:` re-enters `layout` synchronously, so that recurses.
- **Sidebar clears the traffic lights.** The `.sidebar` vibrancy fills the pane, but the outline scroll view is inset
  `crate::TITLE_BAR_HEIGHT` (32pt) from the top so no row sits under the traffic-light strip.

## Browse UI architecture: render from state via `sync_native`

**The whole browse UI is rendered from `browser::State`, the single source of truth.** No native view decides anything;
every browse-UI change follows one rule: **mutate state → call `sync_native(window)`**. `sync_native` is the one
idempotent choke-point ("render") that reads state and sets ALL derived native UI; it's safe to call any number of
times. This replaced an earlier event-driven model where each event poked native views ad-hoc, which let the native
state drift out of sync (a click updated one pane's emphasis but not the other; Tab to the grid left it first responder
with no selection anchor so arrows were dead until you clicked). There's no observer/event-bus — the choke-point IS the
subscription.

**State (the source of truth):** `mode: ViewMode`, `focused_pane: Option<PaneSide>` (`None` in image mode), the tree's
`selected_folder`, and the grid's `grid_selected`. Focus is **never** inferred from the native first responder.

**What `sync_native` reads → sets** (`mod.rs::State::sync_native`):

- `mode` → split-view visibility (shown iff Browse), the wgpu Metal layer hidden iff Browse, and the image title/zoom
  labels hidden iff Browse (`window::set_titlebar_labels_hidden`).
- `focused_pane` → the window's first responder: the focused pane's native control in browse (the `BrowseOutlineView`
  for Tree, the `BrowseCollectionView` for Grid, via `split.apply_focus`), or the winit content view in image mode
  (`window::restore_content_view_first_responder`, so winit owns the keyboard again).
- **Grid-selection invariant:** if the grid is the focused pane and has images but no live selection, seed one
  (`grid_selected`, else 0) before making it first responder. A focused collection view with no selection has no anchor,
  so arrow keys do nothing — this guarantees an anchor, fixing "Tab to the grid leaves arrows dead until you click a
  thumbnail".
- Emphasis: the tree source list draws accent-blue while it's first responder (automatic once the responder follows
  state); `split.apply_focus` calls `BrowseGrid::set_focused(focused == Grid)`, which stores the intended focus on the
  grid's data source and repaints the visible selected items blue-iff-focused. **The grid item reads this state-driven
  flag (`GridDataSource.focused`, queried via the `gridPaneIsFocused` selector), NOT the native first responder** — the
  click→`BrowseSelectFolder`/`BrowseGridSelected` dispatch is async, so reading `window.firstResponder` during a focus
  flip was racy and left the grid drawn blue after a tree click (every focus path funnels through `sync_native`, but the
  emphasis still has to be derived from `focused_pane`, not inferred from the responder).

**Mutation sites that funnel through `sync_native`** (each: set state fields, then `sync_native`):

- `enter_browse` / `enter_image` (`set_view_mode` in `app.rs`) — set `mode` + `focused_pane`. `enter_browse` also grows
  the window to the browse minimum first (see below).
- `toggle_focus` (Tab) — flips the pane via `next_focused_pane`.
- `set_grid_selected` (a grid click/selection → `BrowseGridSelected`) — `focused_pane = Grid`,
  `grid_selected = clicked`.
- `set_tree_focused` (a tree selection → `BrowseSelectFolder`) — `focused_pane = Tree`.
- `grid_folder_listed` (a background listing finished) — a listing flips the grid empty↔non-empty, so re-syncing
  re-derives focus + the grid-selection anchor.

The field-only transition cores (`enter_browse_state` / `enter_image_state` / `toggle_focus_state` / `focus_grid_state`,
plus the pure `next_focused_pane` / `browse_entry_pane`) are headless-tested; the objc2 render in `sync_native` is
covered by the smoke run + live QA.

**Why the native responder chain works here.** In browse mode the GPU layer is hidden and the app stops requesting
redraws, so winit is idle and does NOT re-assert first responder. The focused native view holds the window's first
responder and handles its own keys.

**On entering image mode**, `sync_native` hands first responder back to winit (`restore_content_view_first_responder`):
the hidden outline view would otherwise keep the responder and swallow image-mode keys, so Enter→browse would work only
once. **Don't drop this** — it's why the Enter → browse → Esc → image → Enter cycle repeats. (The labels' visibility is
re-asserted by `set_view_mode` against the title-bar/fullscreen state — `sync_native` only ever _hides_ them in browse,
never forces them on, so a title-bar-off/fullscreen setting wins.)

**Keys via the focused view's `keyDown:` override, not winit.** `BrowseOutlineView` and `BrowseCollectionView` subclass
their controls and override `keyDown:` to intercept only Tab → `ToggleBrowseFocus`, Enter (Return/keypad-Enter) →
`BrowseOpenSelected` (opens the selected image when the grid is focused, else falls back to image mode — so Enter on the
tree returns to the viewer), Esc → `EnterImageMode`. Everything else (arrows, page keys, type-select) calls `super`, so
native selection/scroll stays immediate. The map is the pure `browser::browse_keydown_command(key_code)`, routed via
`crate::commands::send_command`. A defensive `input::browse_key_to_command` still maps Tab/Enter/Esc in case winit ever
delivers a key in browse mode, but with first responder held by the native view it normally doesn't fire. **No
winit-routed browse arrow handling exists** (arrows are native).

**Don't route browse keys through winit.** The native `keyDown:` override IS the browse key path; winit is idle in
browse (layer hidden, no redraws) so it doesn't re-assert first responder and the focused control keeps it. An early
spike appeared to show "winit keeps receiving all keys in browse mode," but that was a stub artifact (the spike never
handed first responder to a native view) — it does NOT describe the shipped model. So don't grow
`input::browse_key_to_command` into a real browse keymap or add winit-side arrow handling; that would fight the native
responder, not help it. Keep the winit map a Tab/Enter/Esc safety net only.

`SharedAppState` exposes the full browse picture at `GET /state` so QA/tests can assert it without keystrokes or
screenshots: `view_mode`, `focused_pane` (`"tree"`/`"grid"`/`"none"`), `browse_selected_folder`, `browse_grid_selected`,
`browse_grid_count` (the listed folder's supported-image count), and `browse_reveal_pending` (the tree's async reveal
walk is in flight — the barrier tests poll on). The QA `SendKey` path maps only Tab/Enter/Esc in browse mode (arrows are
native, so the QA path can't drive native selection by key); for the rest, test-only driving hooks
(`POST /browse/select-folder`, `/browse/select-grid`, `/browse/open`) drive tree selection, grid selection, and open,
since the QA path can't synthesize native outline/collection-view clicks. `qa_select_grid_index` updates the grid model
the way the native `didSelectItemsAtIndexPaths:` delegate does (so the open path reads the right image). See
`qa/CLAUDE.md`.

## The thumbnail grid

`grid.rs` builds the right pane: an `NSCollectionView` (`NSCollectionViewFlowLayout`, vertical scroll) of fixed-size
square cells (`CELL_PT`, see "Styling constants"), each a `GridItem` (an `NSCollectionViewItem` subclass with a
proportionally-scaling `NSImageView` + a filename label). The grid uses a fixed cell size; a bottom slider to
live-resize cells is deferred (see "Deferred work + known limitations"). The grid sits on a rounded gallery surface (see
`split_view.rs`).

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
- **Reveal → image mode (INSTANT, never blocks on decode).** Double-click (detected in `GridItem::mouseDown:` via
  `clickCount == 2` — not a click gesture recognizer, which delays the single click ~600 ms) or Enter on the focused
  grid fires `BrowseOpenSelected`; Esc fires `EnterImageMode`. Both — plus the Browse→image menu toggle — route to
  `App::reveal_selected_image` (Esc == Enter == reveal). Single click selects instantly. Reveal builds a
  `DirectoryList::from_explicit` from the grid's image list (same `SortBy`, so the order matches), positions it at the
  selected index (`resolve_reveal_index`), seeds previews, then hands off to `App::display_open_target` — the **same
  async display path image-mode navigation uses for a cache miss** — wrapped in the black-not-stale reveal (see "The
  swap" above):
  - **Cache hit** (the common case — the selection was warmed, see below): display from cache immediately, then
    `warm_initial_neighbors` tops up the arrow-key window.
  - **Cache miss**: set `pending_current`, show a correct-aspect placeholder instantly (the grid thumbnail's QuickLook
    preview via `display_preview_placeholder`, else a metadata-only `apply_preview_auto_fit` followed by
    `renderer.clear_image()` so the new geometry shows clean BLACK rather than the previous image stretched to it) + a
    "Loading…" title, and `prioritize_target` the full decode on the preloader. The sharp image swaps in when
    `poll_preloader` sees `Ready` for `pending_current`, which also queues neighbors. **No blocking `display_image` on
    the main thread.**

  With no grid selection (Esc/Enter on the tree, or an empty folder), reveal keeps the currently displayed image.

- **Warming the browse selection (so reveal is actually instant).** When the browse selection lands on an image — a grid
  click (`BrowseGridSelected`) or the seeded selection after a folder lists (`BrowseFolderListed`) — the executor calls
  `App::warm_browse_selection`: it reads the grid's image list + selected index (`State::grid_warm_target`, focus-
  independent), computes the prospective-current + `preloader::preload_count()` neighbor indices each side
  (`browser::browse_warm_indices`, which takes the radius as a parameter so it stays pure and host-RAM independent,
  built on `navigation::wrap::active_preload_indices`, clamped to the folder, no wrap), maps them to paths, and warms
  them into the shared `navigation::image_cache` via `Preloader::warm_paths`. The browse selection IS the prospective
  current image (reveal makes it current). **Warming runs by PATH and deliberately does NOT display the image or
  auto-fit the window while browsing** — doing so would resize the window behind the browse UI. The cache is path-keyed,
  so warming arbitrary paths is safe; `warm_paths` cancels paths that drop out of the new set, so a moved selection
  cancels its stale warms. Net effect: by reveal time the full image is usually already cached → instant; and after
  revealing, arrowing left/right is warm.

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
  cache's byte bookkeeping, then **feeds `evict_to_budget()`'s returned indices to `Scheduler::uncache`** (the
  cache/scheduler invariant: an evicted thumbnail must be re-requestable) and drops their `NSImage`s, and reloads the
  affected items so cells pick up their image.
- **Pure plumbing.** `grid_scheduler::Scheduler` (visible-range-centered order, `MARGIN` = 100 cap) and
  `thumbnail_cache::ThumbnailCache` (128 MB byte budget, farthest-from-range eviction, ties by least-recently-touched)
  stay pure and unit-tested; `grid.rs` is their runtime caller. Both share `grid_scheduler::distance_from_range` so
  scheduling and eviction agree.

**Size constant:** `thumbnail_cache::MAX_CELL_PT` (256pt) → `GRID_THUMBNAIL_PX` (512px = 256 × 2 Retina). One max-size
RGBA8 thumbnail is `512 × 512 × 4 ≈ 1 MB` (`EST_THUMBNAIL_BYTES`), so 128 MB ≈ 128 resident thumbnails. Generated
**once** at this size; smaller cells downscale the cached bitmap — never regenerated on resize.

## Live folder sync (browse mode)

Browse stays live on disk changes via the shared `crate::folder_watch` infra (FSEvents watcher + coalescer + off-thread
re-scan). Two independent watches:

**Grid (active-folder image-list watch).** The active-folder watch follows **the grid's listed folder** in browse (the
current image's folder in image mode); `App::active_folder` picks by mode and `retarget_active_folder_watch` re-targets
on every mode switch, on `BrowseFolderListed` (the grid's folder just changed), and on image open. A `FolderChanged` for
the grid's folder re-scans off-thread; `App::apply_folder_rescan` then drives `State::apply_grid_rescan` →
`BrowseGrid::apply_rescan`: insert adds at the sorted position, drop removes, **keep the selection by path** (pure
`grid_model::select_after_rescan` — next/previous surviving image when the selected file was deleted, `None` for an
emptied folder → "(No images)"), and refresh thumbnails (a full clear-and-repump regenerates modified/added cells from
the shared QuickLook cache). A changed selection re-warms via `warm_browse_selection`. The same re-scan also updates the
image-mode `dir_list` when the folder is the open image's folder, so synced modes stay coherent (see
`navigation/CLAUDE.md` → "Live folder sync (image mode)").

**Tree (folder-structure watch).** Roots are watched for the window's life (`App::watch_tree_roots`, called when the
split view is first built). Each folder is watched on expand (`outlineViewItemDidExpand:` →
`AppCommand::BrowseTreeFolderExpanded` → `App::watch_tree_folder`) and unwatched on collapse
(`outlineViewItemDidCollapse:` → `BrowseTreeFolderCollapsed` → `App::unwatch_tree_folder`). Collapse never unwatches a
**root** or a folder still serving as the active image-list folder (a folder can be both). Bounded: only roots +
expanded folders are watched, never the whole disk.

A `FolderChanged` for a watched tree folder re-scans its subdirectories — `State::reload_tree_node` →
`BrowseTree::reload_node` invalidates the `ChildCache` entry and re-scans via the existing async `TreeScanner`. The
re-scan completion (`outline::BrowseTree::reload_completed`) is **subdir-delta-gated and selection-preserving** — both
matter because revealing a folder expands and watches its ancestors, and a busy ancestor (`/tmp`, Downloads) fires
file-change events constantly:

- **Reacts to sub-folder changes only, never file changes.** The tree shows directories only, so the completion diffs
  the fresh subdir set against what the node showed before (pure `subdirs_changed`) and `reloadItem:reloadChildren:`s
  **only if a subfolder was actually added or removed**. A folder whose files changed but subdirs didn't does nothing to
  the tree. Without this gate, a busy ancestor reloads constantly. (The active-folder _image_ watch still handles file
  changes for the grid/sequence — separate, and unaffected.)
- **A reload never corrupts the selection.** `reloadItem:reloadChildren:` can drop the selected descendant's selection
  even when it still exists. The completion restores it: the pure `selection_action_after_reload` decides from the live
  tree (does the selected path still have a row?) → re-select the same folder if it survives (`Restore`, via
  `reselect_preserving` with `suppress_selection_command` set, so NO `BrowseSelectFolder` / grid re-list fires), or
  select the reloaded parent only when the folder is **genuinely gone from disk** (`SelectParent` — the only case that
  re-lists). A routine reload where the selection still exists never trips the fallback.

This fixes a live-QA runaway: a reveal under a busy ancestor (`/private/tmp`) used to fire `FolderChanged` on the
ancestor, reload it, drop the deep selection, and trip the select-parent fallback — jumping the selection up to the busy
ancestor and listing _its_ images with zero user action.

**Routing.** One `FolderChanged { folder }` can match BOTH roles, and both fire (`App::handle_folder_changed` runs the
tree-node reload and the active-folder re-scan independently — neither is `else` to the other). The objc2
expand/collapse and reload are covered by smoke + live QA; the grid add/remove/modify with selection-by-path is covered
by the `live_sync_browse_grid_*` integration tests and the pure `select_after_rescan` unit tests.

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
  zero — a gray void. Set an initial divider position with `setPosition:ofDividerAtIndex:` so neither pane starts
  collapsed.
- **Build on the main thread.** `BrowseSplitView::create` asserts the `MainThreadMarker`; it's only ever called from the
  winit event loop (main thread).

## Styling constants (the baseline gallery look)

The look is tuned via named constants so it's easy to refine; values are logical points unless noted. All colors are
semantic `NSColor`s (light/dark adapt automatically). Tweak these, not magic numbers buried in calls.

- **Grid cells** (`grid.rs`): `CELL_PT` 168 (cell side), `CELL_IMAGE_PT` 140 (centered square thumbnail),
  `CELL_LABEL_PT` 18 + `CELL_IMAGE_LABEL_GAP_PT` 6 (filename label below), `CELL_LABEL_FONT_PT` 11 (small system font,
  `secondaryLabelColor`, single-line middle-truncation), `CELL_SPACING` 16 (inter-cell, both axes), `SECTION_INSET_PT`
  14 (inset inside the gallery surface). The cell container is a `FlippedView` so the thumbnail-on-top / label-below
  layout reads top-down.
- **Selection ring** (`grid.rs`): `SELECTION_CORNER_RADIUS` 8, `SELECTION_FOCUSED_ALPHA` 0.85 (softens the focused
  accent fill). Focus model is unchanged: focused pane → `selectedContentBackgroundColor` (accent), unfocused →
  `unemphasizedSelectedContentBackgroundColor` (gray), driven by `gridPaneIsFocused` (see "Selection emphasis follows
  focus"). `SELECTION_INSET_PT` 2 is reserved for a future inset ring.
- **Empty state** (`grid.rs`): `EMPTY_LABEL_FONT_PT` 15, `secondaryLabelColor`, centered on the gallery surface.
- **Gallery surface** (`split_view.rs`): the grid sits on a rounded-corner `controlBackgroundColor` container inset from
  the grid pane — `GALLERY_INSET_PT` 10 (leading/trailing/bottom), `GALLERY_TOP_INSET_PT` (= `TITLE_BAR_HEIGHT` + inset,
  clears the traffic-light strip), `GALLERY_CORNER_RADIUS` 10 (`masksToBounds` clips the grid to the rounded corners).
- **Sidebar rows** (`outline.rs`): `ROW_HEIGHT_PT` 24, `ICON_SIZE_PT` 16, `INDENT_PER_LEVEL_PT` 14, `ICON_LABEL_GAP_PT`
  6, `CELL_LEADING_INSET_PT` 2, `CELL_TRAILING_INSET_PT` 4, `LABEL_FONT_PT` 13 (system font, middle-truncating). Keeps
  the `.sidebar` vibrancy + source-list rounded-pill selection.

## Deferred work + known limitations

- **Bottom thumbnail-size slider — deferred (not built).** A slider at the window bottom to live-resize grid cells is
  the one piece of the original design not shipped. The sizing rule already supports it with zero new generation:
  thumbnails are generated **once** at `MAX_CELL_PT × 2` px (see "Size constant"), so a smaller cell just downscales the
  cached bitmap — a future slider changes the flow-layout item size only, never re-requests QuickLook.
  `SELECTION_INSET_PT` and `CELL_PT` are fixed for now. Build it in a styling pass if it earns its place; the constraint
  to keep is "generate at the max, downscale below it" so adding the slider stays regeneration-free.
- **Linux has no browser.** macOS and Windows each have a native one; Linux has the shared models and no shell, so the
  mode-toggle commands exist and `enter_image` via plain `set_view_mode(Image)` is the fallback. A single cross-platform
  browse mode drawn in wgpu is explicitly out of scope (`docs/specs/image-browser.md` → "Why native AppKit", and
  `docs/specs/windows-ui-design.md` → "Drawing the chrome ourselves in wgpu"). A Linux one is M8's to spec.
- **The thumbnail-size slider is deferred on Windows too.** When it lands there, Explorer's own vocabulary is the
  control a Windows user knows: a View submenu of Extra large / Large / Medium / Small icons rather than a slider.
- **Far-jump thumbnail repopulation.** Same trade-off as previews: scrolling far past the cached window shows brief
  placeholders while the visible-centered scheduler regenerates from Finder's warm QuickLook cache (~150 ms each on a
  revisit). Expected, matches Finder/Photos.
