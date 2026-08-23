# Prvw: image browser (browse mode)

A second top-level screen for the main window. Image mode (today's wgpu viewer) stays exactly as-is. Browse mode is a
native AppKit screen: a folder tree on the left, a thumbnail grid on the right. The two screens **swap**, they never
overlap.

Status: shipped, bar a dedicated styling pass (the "beautiful gallery" look, reviewed visually with David). The left
pane is a live `NSOutlineView` source list (home + mounted volumes, **asynchronous** directory enumeration,
path-identity nodes, arrow-key nav, selection recorded in `browser::State`). The right pane is a live `NSCollectionView`
thumbnail gallery (`browser::grid`): selecting a folder lists its supported images **on a background worker**
(`browser::grid_listing`, never the main thread), the grid populates and shows QuickLook thumbnails generated via a
second `previews::quicklook` request path (RGBA8 → `NSImage` through `quicklook::nsimage_from_rgba8`), driven by the
visible-range scheduler + 128 MB byte-budget cache. Native click selects; double-click or Enter (grid focused) opens
that image in image mode (sets up `navigation`, switches mode); an empty folder shows "(No images)" and is non-focusable
so Tab stays on the tree. Browse-open positioning, the async reveal-path walk, dir-arg startup, and arrow-key pane
isolation are all in place; the QA `/state` snapshot and integration tests assert the full flow. See
`apps/desktop/src/browser/CLAUDE.md` for the tree, grid, thumbnail, and browse-open positioning details.

## Why native AppKit, not wgpu

The tree is `NSOutlineView` in an `NSScrollView` (source-list style); the grid is `NSCollectionView`. Both give native
scrolling, selection, keyboard nav, cell reuse, and accessibility for free, fed by macOS's own QuickLook thumbnails. A
wgpu grid would reimplement all of that and force continuous redraws while scrolling, which fights the "respect
resources" principle. So browse mode is fully native; image mode is fully GPU. This is also why they swap instead of
compositing: a transparent Metal pixel still **occludes** in-window content behind it (see the "Native AppKit views
over/around the wgpu Metal layer" gotcha in `apps/desktop/src/platform/macos/CLAUDE.md`), so we can't layer native views
under the live surface. We hide one and show the other.

## The swap mechanism

- The browse UI is an `NSSplitView` added as a **sibling subview of winit's contentView** (same pattern as
  `window::add_titlebar_labels`: `addSubview`, layer-backed, `zPosition` raised above the Metal layer's `1.0`), pinned
  to the contentView edges with Auto Layout, with a stable `identifier` for lookup + hide/show.
- **Enter browse:** clear the bound image and paint one black frame while the Metal layer is still visible (so its
  last-composited frame is black, see "the reveal is instant and black-not-stale" below), unhide the split view, hide
  the `CAMetalLayer` (`setHidden: true` via the existing `find_metal_layer` walk), stop requesting redraws.
  Render-on-demand means the GPU goes fully idle.
- **Enter image (reveal):** hide the split view, unhide the Metal layer, then synchronously paint the target image.
- Every `Retained<>` (split view, both scroll views, outline view, collection view, data sources, delegates) is owned by
  the view hierarchy / a `Vec` that outlives the window (no early drop → no autorelease segfault). These are subviews,
  not a modal session, so the "no AppKit modals in the winit loop" rule does not apply.

## Layout and look

- **Left pane — folder tree.** `NSVisualEffectView` `.sidebar` material behind an `NSOutlineView` with
  `selectionHighlightStyle = .sourceList` (this is where the rounded-pill selection comes from). Directories only, no
  files. Lazy disclosure. Roots: the home folder and every mounted volume as **flat sibling rows** — a Finder-style
  "Locations" group header needs group-row pseudo-items that fight the path-keyed node model, so flat roots win (see
  `browser/CLAUDE.md` → "The folder tree"). Grouped rows could return in the styling phase.
- **Right pane — thumbnail grid.** `NSCollectionView` with a flow layout, styled into a polished gallery (rounded
  container background like Finder's content area, comfortable cell spacing, native hover/selection). Vertically
  scrollable. Shows the images of the folder selected in the tree, in the existing sort order (`navigation::SortBy`).
- **Empty folder:** a centered "(No images)" label in the grid area, and the grid **cannot receive focus** (Tab stays on
  the tree).
- Baseline first, then a dedicated styling pass with David's visual review (positioning/spacing nuance is reviewed by
  human eye, not agents).

## Behavior

- **Tree is browse-only.** Image mode is unchanged; no tree there.
- **Entering browse** (from image mode via Enter or Navigate → Image browser): reveal + select the current image's
  folder in the tree and scroll it to roughly mid-view; list that folder's images; preselect the current image in the
  grid (blue) and scroll it into view; focus the grid. The grid drives the image-mode current (`resolve_reveal_index`),
  so Esc/Enter right after opening round-trips to the same image. The reveal is **async** because child directories load
  on the background scanner: `tree_model::reveal_path_chain` computes the root-to-folder chain (longest-prefix root, so
  a path under home reveals under Home, not the `/` volume), and a pending `RevealWalk` in `outline::BrowseTree` expands
  one ancestor per `BrowseTreeChildrenLoaded` until the target is reached, then selects it — never blocking the main
  thread. With the selected folder empty, focus falls back to the tree. See `browser/CLAUDE.md` → "Browse-open
  positioning".
- **Esc == Enter == reveal the browse-selected image.** The user's model: the image-mode current image IS whatever the
  browse cursor points at, even while the Metal canvas is hidden. So Esc, Enter (on the focused grid), a double-click,
  and Navigate → Image view all do the **same** thing — reveal image mode showing the currently-selected browse image.
  They share one path (`App::reveal_selected_image`); there's no separate "Esc preserves the previously displayed image"
  behavior. With no grid selection (tree focused, or an empty folder), reveal degrades gracefully: it shows the last
  valid image (whatever was current), never a blank or stale frame.
- **The reveal is instant and black-not-stale — never the previous image.** Two compositor facts shape this: presenting
  a frame to a HIDDEN `CAMetalLayer` doesn't commit (it keeps its last-visible frame until ~100 ms later), so painting
  while hidden then unhiding can't kill the stale frame; and a new image's auto-fit geometry must never composite the
  old texture (it would stretch it distorted). So: on entering browse, the app clears the bound image and paints one
  BLACK frame while the layer is still visible (the split view covers it instantly, so it's unseen) — making the layer's
  last-visible content black. On reveal, the app points navigation at the grid's folder + selected index, UNHIDES the
  layer, then synchronously paints the target. If the full image is cached (the common case — the selection is warmed,
  see below) the correct image lands in that one frame (black → image); on a cache miss the grid thumbnail's correct-
  aspect QuickLook preview shows with a "Loading…" overlay (or clean black if no preview is cached) and the sharp full
  decode swaps in via the preloader. The renderer fills the image area with opaque black whenever no image is bound, so
  the worst the user ever sees is a brief black, never the stale stretched previous image. It never blocks the main
  thread on a full decode; it reuses the same async cache-hit / cache-miss display path image-mode navigation uses.
- **Selecting a folder** in the tree lists its images in the grid.
- **The browse selection drives the prospective current image and is warmed.** When the selection lands on an image
  (grid click or arrow-key selection), the app treats it as the prospective current image and warms it plus
  `preloader::preload_count()` neighbors each side into the shared image cache via the preloader, cancelling/replacing
  as the selection moves (standard image-mode preloader behavior). That radius is the image-mode preload window, which
  narrows on a machine whose cache budget can't retain the full one — warming fills the same cache, so warming wider
  than it retains would evict on arrival. Warming is **by path** and does not display the image or auto-fit the window
  while browsing (which would resize the window behind the browse UI) — the reveal makes it current and paints it. By
  reveal time the full image is usually already cached (instant), and arrowing in image mode afterward is warm.
- **Tab** toggles focus between tree and grid (app-managed in `browser::State`, not the native key-view loop — see
  "Input architecture"; the grid is skipped when empty). All focus paths (Tab, a tree click, a grid click) funnel
  through `sync_native`, which derives the grid's selection-emphasis color from `focused_pane` state (not the native
  first responder, which is racy during the async click→command flip) — so a tree click grays the grid item and a grid
  click blues it, matching Tab.
- **Esc** in browse mode reveals the selected image (see above).
- **Enter is reassigned in image mode.** Today Enter toggles fullscreen (alongside `f`/`F11`); it now enters browse mode
  instead. `f` and `F11` keep toggling fullscreen, so the capability isn't lost — only Enter's binding moves.
- **Menu:** new "Image browser" / "Image view" item at the **top of the Navigate menu** with a separator under it. One
  item, label flips by mode (same `set_text()` pattern the slideshow Start/Stop item uses).
- **Startup:** an image argument → image mode (unchanged). A **directory argument** → browse mode, that directory
  revealed + selected in the tree (reusing the browse-open reveal walk) and its images listed; the grid focuses with the
  first image preselected, or the tree for an empty folder. No argument (or a missing/unreadable path) → today's
  behavior (onboarding). The file-vs-dir-vs-onboarding split is the pure `browser::classify_launch_target`; `main.rs`
  detects a lone directory arg and `app.rs`'s `initialize_viewer` boots browse mode instead of displaying an image.

## State

A per-feature `browser::State` (sibling of `zoom::State`, `navigation::State`, etc.) holding the current
`ViewMode { Image, Browse }`, `focused_pane`, the tree's selected folder, the grid's selected index, and the native view
handles (behind macOS `cfg`). `App` delegates to it. The grid's selection and `navigation::DirectoryList::current_index`
are kept coherent so switching modes lands on the right image and the existing preloader warms neighbors.

**QA observability.** `SharedAppState` (read by the QA server at `GET /state`) mirrors the browse picture so tests and
tools can assert it without keystrokes or screenshots: `view_mode`, `focused_pane` (`"tree"`/`"grid"`/`"none"`),
`browse_selected_folder`, `browse_grid_selected`, `browse_grid_count` (the listed folder's supported-image count), and
`browse_reveal_pending` (the tree's async reveal walk is in flight — the barrier tests poll on). The QA server also adds
test-only driving hooks (`POST /browse/select-folder`, `/browse/select-grid`, `/browse/open`) so the integration tests
can drive the flow headlessly, since the QA path can't synthesize native outline/collection-view clicks. See
`apps/desktop/src/qa/CLAUDE.md` and `docs/mcp-server.md`.

## Input architecture

In browse mode the GPU layer is hidden and the app stops requesting redraws, so **winit is idle and does not re-assert
first responder.** That lets the focused native view hold the window's first responder and handle its own keys
(verified: `makeFirstResponder` is accepted, and winit doesn't re-assert during browse).

So browse mode uses the AppKit responder chain, with one app-level source of truth and a single render step:

- **`browser::State` is the ONLY source of truth for the browse UI**: `mode`, `focused_pane: Option<PaneSide>` (`None`
  in image mode, `Some(Tree)`/`Some(Grid)` in browse), the tree's selected folder, and the grid's selected index.
  Nothing else decides focus/emphasis; focus is never inferred from the native first responder.
- **One idempotent `sync_native(&self, window)` ("render") sets ALL derived native UI from state**, safe to call any
  number of times. It derives, from `mode`: split-view visibility, the wgpu Metal layer hidden flag, and the image
  title/zoom labels hidden flag; from `focused_pane`: the window's first responder (`makeFirstResponder:` the focused
  pane's `NSOutlineView`/`NSCollectionView` in browse, or the winit content view in image mode) and the grid's per-item
  emphasis (tree emphasis follows first responder automatically). It also enforces the **grid-selection invariant**: a
  focused, non-empty grid with no live selection gets one seeded (so arrow keys always have an anchor — no "arrows dead
  until you click").
- **Every mutation goes through state then `sync_native`** — the one choke-point, no observer/event-bus. `enter_browse`
  (focus the grid if the selected folder has images, else the tree; also grow the window to a sensible browse minimum if
  it shrank for a small image), `enter_image` (`focused_pane = None`), `toggle_focus` (Tab: toggle Tree↔Grid, skipping
  an empty grid), a grid click (focus Grid + record the index), a tree selection (focus Tree), and a completed folder
  listing all set state then render. This structurally prevents the native UI from drifting from state (the bug an
  earlier per-event-emphasis model had: a click updated one pane's emphasis but not the other).
- **Keyboard via the focused view's `keyDown:` override, not winit.** `BrowseOutlineView` and `BrowseCollectionView`
  subclass their controls and override `keyDown:` to intercept only Tab → `ToggleBrowseFocus`, Enter (Return/keypad) →
  open-selected (Grid) or return-to-image (Tree), Esc → `EnterImageMode`. Everything else (arrows, page keys,
  type-select) calls `super`, so native selection/scroll stays immediate. The map is `browser::browse_keydown_command`,
  routed via `crate::commands::send_command`. There is **no** winit-routed browse arrow handling.
- **Emphasis follows focus** (set by `sync_native`). Tree: an `NSOutlineView` source list draws accent-blue selection
  when it's first responder and gray otherwise — so syncing first responder to `focused_pane` makes it correct
  automatically. Grid: each `GridItem` draws its selection as a rounded rect — accent-blue when the grid is the focused
  pane, gray when selected but not, nothing when not selected. The focused-or-not flag is read from
  `browser::State::focused_pane` (mirrored onto the grid's data source by `BrowseGrid::set_focused`), **not** inferred
  from the native first responder: the click→command focus flip is async, so reading `window.firstResponder` during it
  is racy and left the grid drawn blue after a tree click. `set_focused` repaints the visible selected items on every
  focus change.
- **Mouse is fully native:** single-click selects instantly (focusing the grid), double-click opens (detected in
  `GridItem::mouseDown:` via `clickCount == 2`, so no click-delay), and scroll-wheel scrolls — all native.
- **Restore the content view as first responder when leaving browse.** On Esc → image the hidden outline view can keep
  the responder, so winit never sees the next key (image-mode Enter does nothing). `sync_native` in image mode calls
  `window::restore_content_view_first_responder` so the Enter → browse → Esc → image → Enter cycle repeats. Don't drop
  it.
- A defensive `input::browse_key_to_command` fallback still maps Tab/Enter/Esc in case winit ever delivers a key in
  browse mode, but with first responder held by the native view it normally doesn't fire.

## Thumbnails: the `previews` module, reused QuickLook, AppKit-owned memory

The module is named `previews` (not `thumbnails`) because what it generates are ~1024px (512pt × retina) QuickLook
_previews_ used as soft placeholders during full-image decode, not grid thumbnails. Apple's own names stay verbatim
(`QLThumbnailGenerator`, `QLThumbnailGenerationRequest`, `QLThumbnailGenerationRequestRepresentationTypes::Thumbnail`,
the `objc2-quick-look-thumbnailing` crate, ImageIO keys) — the rename is ours, the framework's vocabulary isn't.

The QuickLook generator is shared, size-parameterized infrastructure: both the preview path (requests ~1024px) and the
grid (requests grid size) call the same `quicklook::RequestTable`. `QLThumbnailGenerator` _is_ Finder's system cache
(`quicklookd`, shared, warm), so the grid is a second request path, not a second engine.

**Grid thumbnails are AppKit-owned, generated once at the slider max, never regenerated on resize.**

- **Size:** always generate at `max_cell_pt × 2` physical px. With the slider max at **256pt that's 512px ≈ 1 MB** per
  thumbnail. Smaller cell sizes **downscale** the cached bitmap (instant, crisp); the slider never re-requests. A larger
  max means bigger thumbnails and more memory, so 256pt is the ceiling.
- **No Rust pixel copies.** Grid cells are `NSImageView`s consuming `NSImage` straight from QuickLook; the bitmaps live
  AppKit-side, owned by the cells, released on cell reuse. (Previews copy into `Vec<u8>` only because they upload to a
  wgpu texture; the grid doesn't.)
- **Memory = visible set + margin, capped.** `NSCollectionView` cell reuse releases off-screen images, so resident
  memory tracks the visible set, not the folder size — a 10k-image folder costs the same as a 50-image one. A **128 MB**
  LRU backstop (≈128 resident thumbnails, centered on the visible range) bounds the small-cell worst case (many tiny
  cells visible at once).
- **Smooth scrolling via a visible-centered scheduler.** Same pattern as the `previews` scheduler: the folder is
  enqueued prioritized **centered on the visible range, nearest-first**, **re-prioritized live on scroll** so whatever
  is on-screen always generates first. `NSCollectionViewPrefetching` warms a one-screen margin ahead/behind. Evicted
  thumbnails re-request fast from Finder's warm cache; fast-scroll past the margin shows a brief placeholder
  (Finder/Photos behavior).

The deferred bottom **slider** (see "Deferred work" in `browser/CLAUDE.md`) would live-resize cells by changing the
flow-layout item size only — the sizing rule above keeps that regeneration-free.

## Component map

What makes up the feature, and where the load-bearing decisions live:

- **Thumbnail plumbing.** The shared, size-parameterized QuickLook generator with a second grid request path; the
  visible-centered scheduler + 128 MB byte-budget cache (headless unit-tested for nearest-first ordering, scroll
  re-prioritization, and eviction).
- **Tree pane.** `NSOutlineView` source-list (`setStyle(.SourceList)`), home + mounted volumes as **flat sibling roots**
  (the grouped "Locations" header fights the path-keyed node model — see `browser/CLAUDE.md`), lazy async directory
  enumeration, path-identity `NodeObject`s, native arrow-key nav, selection → `BrowseSelectFolder`.
- **Grid pane.** `NSCollectionView` wired to the scheduler + cache, async folder listing (`grid_listing`), the
  RGBA8→`NSImage` seam (`quicklook::nsimage_from_rgba8`), native selection (single-click instant, double-click opens via
  `mouseDown:`), focus-aware per-item emphasis, Enter open-to-image hand-off, and the "(No images)" empty state (grid
  non-focusable when empty).
- **Behaviors.** Browse-open folder-reveal + scroll-to-mid + current-image preselect + grid focus via the **async
  reveal-path walk** (`tree_model::reveal_path_chain` + a `RevealWalk` advanced by `BrowseTreeChildrenLoaded`, since
  children load on the background scanner); Tab; double-click/Enter → image; dir-arg startup
  (`browser::classify_launch_target` + `App::launch_directory`); arrow-key pane isolation (automatic from the
  single-first-responder focus model). Pure logic (reveal chain, launch classification, grid-preselect index) is
  unit-tested.
- **QA + tests.** `SharedAppState` browse fields + the test-only QA driving hooks (see the QA observability note in
  "State"); integration tests asserting the full flow.

The remaining piece is the styling pass — the "beautiful gallery" look (`NSCollectionView` polish is craft, not a flag),
reviewed visually with David. The grid uses a fixed cell size until then; the deferred bottom size-slider is tracked in
`browser/CLAUDE.md` → "Deferred work".

## Test plan

- **Headless unit tests:** the scheduler's nearest-first ordering + scroll re-prioritization, the 128 MB cache eviction,
  and the pure browse logic (reveal chain, launch classification, grid-preselect/reveal index, focus transitions).
- **Integration tests** (`tests/integration.rs`, driven through the QA server, macOS-only): mode switch (Enter/Esc),
  dir-arg launch booting into browse with the folder revealed + listed, selecting a folder by path lists its images, the
  empty-folder zero-count + non-focusable grid, Tab focus flips reflected in `focused_pane`, the grid-selection → open
  round-trip, and entering browse from an image preselecting it. They poll `/state` (`browse_reveal_pending == false` +
  `browse_grid_count`) for a non-flaky barrier, each with its own temp `HOME` so the tree's home root scopes a short
  reveal walk.
- **Manual:** focus/Tab/keyboard, dir-arg launch, empty folder, very large folder scrolling, and the styling review.
- `./scripts/check.sh` (all checks) green before every commit.
