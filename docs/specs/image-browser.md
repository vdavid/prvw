# Prvw: image browser (browse mode)

A second top-level screen for the main window. Image mode (today's wgpu viewer) stays exactly as-is. Browse mode is a
native AppKit screen: a folder tree on the left, a thumbnail grid on the right. The two screens **swap**, they never
overlap.

Status: in progress on the `image-browser` worktree. Done: Phase 0 (spike), Phase 1 (thumbnails → previews rename),
Phase 3 (real folder tree). The left pane is now a live `NSOutlineView` source list (home + mounted volumes, lazy
directory enumeration, path-identity nodes, arrow-key nav, selection recorded in `browser::State`). Still stubbed: the
right-pane grid (Phase 4) and Phase 2 thumbnail plumbing. See `apps/desktop/src/browser/CLAUDE.md` for the tree details.

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
- **Enter browse:** unhide the split view, hide the `CAMetalLayer` (`setHidden: true` via the existing
  `find_metal_layer` walk), stop requesting redraws. Render-on-demand means the GPU goes fully idle.
- **Enter image:** hide the split view, unhide the Metal layer, `request_redraw()`.
- Every `Retained<>` (split view, both scroll views, outline view, collection view, data sources, delegates) is owned by
  the view hierarchy / a `Vec` that outlives the window (no early drop → no autorelease segfault). These are subviews,
  not a modal session, so the "no AppKit modals in the winit loop" rule does not apply.

## Layout and look

- **Left pane — folder tree.** `NSVisualEffectView` `.sidebar` material behind an `NSOutlineView` with
  `selectionHighlightStyle = .sourceList` (this is where the rounded-pill selection comes from). Directories only, no
  files. Lazy disclosure. Roots: the home folder and every mounted volume. Finder-style group rows ("Locations" for
  volumes) are the target; if grouping fights the data source, fall back to flat sibling roots (home + volumes).
- **Right pane — thumbnail grid.** `NSCollectionView` with a flow layout, styled into a polished gallery (rounded
  container background like Finder's content area, comfortable cell spacing, native hover/selection). Vertically
  scrollable. Shows the images of the folder selected in the tree, in the existing sort order (`navigation::SortBy`).
- **Empty folder:** a centered "(No images)" label in the grid area, and the grid **cannot receive focus** (Tab stays on
  the tree).
- Baseline first, then a dedicated styling pass with David's visual review (positioning/spacing nuance is reviewed by
  human eye, not agents).

## Behavior

- **Tree is browse-only.** Image mode is unchanged; no tree there.
- **Entering browse** (from image mode via Enter or Navigate → Image browser): select the current image's folder in the
  tree and scroll it to roughly mid-view; list that folder's images; select the last-displayed image in the grid; focus
  the grid.
- **Leaving browse to a specific image:** double-click or Enter on a grid item → image mode showing that image. Enter on
  the tree, or Navigate → Image view, also returns to image mode (showing the current image).
- **Selecting a folder** in the tree lists its images in the grid. It does not change "the displayed image" until the
  user opens one.
- **Tab** toggles focus between tree and grid (app-managed in `browser::State`, not the native key-view loop — see
  "Input architecture"; the grid is skipped when empty).
- **Esc** in browse mode returns to image mode (mirrors today's exit-fullscreen-or-app feel; the current image stays).
- **Enter is reassigned in image mode.** Today Enter toggles fullscreen (alongside `f`/`F11`); it now enters browse mode
  instead. `f` and `F11` keep toggling fullscreen, so the capability isn't lost — only Enter's binding moves.
- **Menu:** new "Image browser" / "Image view" item at the **top of the Navigate menu** with a separator under it. One
  item, label flips by mode (same `set_text()` pattern the slideshow Start/Stop item uses).
- **Startup:** an image argument → image mode (unchanged). A **directory argument** → browse mode, that directory
  selected in the tree and its images listed. No argument → today's behavior (onboarding).

## State

A new per-feature `browser::State` (sibling of `zoom::State`, `navigation::State`, etc.) holding the current
`ViewMode { Image, Browse }`, the tree's selected folder, the grid's selected index, and the native view handles (behind
macOS `cfg`). `App` delegates to it. The grid's selection and `navigation::DirectoryList::current_index` are kept
coherent so switching modes lands on the right image and the existing preloader warms neighbors.

## Input architecture (spike finding)

The Phase 0 spike established how the keyboard behaves when the native browse UI is shown, and it is the opposite of the
initial assumption: **winit keeps delivering `WindowEvent::KeyboardInput` even while the native split view is up.**
Proof — in browse mode, Esc reached the app's Exit path and the native pane's `keyDown:` override never fired, even
though `makeFirstResponder` returned `true` (winit re-asserts its own content view as first responder, so a native view
can't reliably hold the keyboard).

So browse mode does **not** use the AppKit responder chain for keys:

- **All keyboard flows through winit → `input` → `AppCommand`, branched by mode.** When `browser.is_browse()`, keys map
  to browse actions (Tab → toggle focused pane, arrows → move selection in the focused native view, Enter → open the
  selected image, Esc → image mode) instead of image-viewer actions. Image mode is unchanged. The `main.rs` Esc
  special-case (fullscreen-or-quit) must also branch: Esc in browse returns to image mode, never quits.
- **Browse-mode keys drive the native views programmatically** (`NSOutlineView` expand/collapse + row selection,
  `NSCollectionView` selection + scroll-to-visible). The views render selection highlight regardless of first-responder
  state, so app-managed focus is enough.
- **Mouse is fully native:** click, double-click-to-open, and scroll-wheel are handled by `NSOutlineView` /
  `NSCollectionView` directly — those work without the responder-chain caveat.
- The spike's `BrowsePane` `keyDown:` subclass is dropped; panes are plain scroll views hosting the native controls.
- Focused pane (for Tab) is tracked in `browser::State`, not the native key-view loop.

More input-routing code than leaning on the native key loop, but deterministic and it doesn't fight winit for first
responder.

## Thumbnails: rename first, reuse QuickLook, let AppKit own the memory

The existing `src/thumbnails/` is a **misnomer** — it generates ~1024px (512pt × retina) QuickLook _previews_ used as
soft placeholders during decode, not grid thumbnails. The work splits cleanly:

1. **Rename `thumbnails` → `previews`** across the codebase (module/dir, types `Thumbnail`/`ThumbnailEvent`/…, fns
   `display_thumbnail_placeholder`/`pump_thumbnail_requests`/…, the `AppCommand::ThumbnailsAvailable` variant, the
   `previews_status` QA tool, fields, comments, docs, `thumbnail-preload.md`, the colocated `CLAUDE.md`). Apple names
   stay verbatim (`QLThumbnailGenerator`, `QLThumbnailGenerationRequest`,
   `QLThumbnailGenerationRequestRepresentationTypes::Thumbnail`, the `objc2-quick-look-thumbnailing` crate, ImageIO
   keys). Pure refactor, zero behavior change — full check suite stays green. A precise ~150-site inventory exists from
   the planning pass.

2. **Extract the QuickLook generator into shared, size-parameterized infrastructure.** It currently lives inside the
   module; both the preview path (requests ~1024px) and the new grid (requests grid size) call it.
   `QLThumbnailGenerator` _is_ Finder's system cache (`quicklookd`, shared, warm), so this is a second request path, not
   a second engine.

3. **Grid thumbnails are AppKit-owned, generated once at the slider max, never regenerated on resize.**
   - **Size:** always generate at `max_cell_pt × 2` physical px. With the slider max at **256pt that's 512px ≈ 1 MB**
     per thumbnail. Smaller cell sizes **downscale** the cached bitmap (instant, crisp); the slider never re-requests. A
     larger max means bigger thumbnails and more memory, so 256pt is the default ceiling.
   - **No Rust pixel copies.** Grid cells are `NSImageView`s consuming `NSImage` straight from QuickLook; the bitmaps
     live AppKit-side, owned by the cells, released on cell reuse. (Previews copy into `Vec<u8>` only because they
     upload to a wgpu texture; the grid doesn't.)
   - **Memory = visible set + margin, capped.** `NSCollectionView` cell reuse releases off-screen images, so resident
     memory tracks the visible set, not the folder size — a 10k-image folder costs the same as a 50-image one. A **128
     MB** LRU backstop (≈128 resident thumbnails, centered on the visible range) bounds the small-cell worst case (many
     tiny cells visible at once).
   - **Smooth scrolling via a visible-centered scheduler.** Reuse the `previews` scheduler pattern: enqueue the folder
     prioritized **centered on the visible range, nearest-first**, and **re-prioritize live on scroll** so whatever is
     on-screen always generates first. `NSCollectionViewPrefetching` warms a one-screen margin ahead/behind. Evicted
     thumbnails re-request fast from Finder's warm cache; fast-scroll past the margin shows a brief placeholder
     (Finder/Photos behavior).

A future **slider** at the window bottom live-resizes cells by changing the flow-layout item size (image views downscale
the cached max-size bitmaps — zero regeneration). Out of baseline scope, but the sizing rule above is chosen to support
it; add it in the styling era if it fits.

## Build order (phases)

Each phase is a focused, checks-green commit on the worktree. I lead; subagents do the legwork; I review every diff and
re-run checks before integrating.

0. **Spike (de-risk, first).** Stub split view + both panes (placeholder content), the menu item + label flip, the mode
   swap (hide/show Metal layer), Tab focus between panes, and double-click/Enter/Esc → mode change wired through
   `AppCommand`. Goal: prove the AppKit-in-the-winit-loop focus/keyboard/swap plumbing with no crashes before investing
   in real content. May evolve into the real implementation rather than be thrown away.
1. **Rename** `thumbnails` → `previews` (independent, mechanical; lands as its own commit).
2. **Thumbnail plumbing:** extract the shared, size-parameterized QuickLook generator, then add the grid request path
   and the visible-centered scheduler. Headless unit tests (TDD) for the scheduler's nearest-first ordering + scroll
   re-prioritization and for the 128 MB budget eviction.
3. **Tree pane** (done): `NSOutlineView` source-list (`setStyle(.SourceList)`), home + mounted volumes as **flat sibling
   roots** (the grouped "Locations" header was the target but fights the path-keyed node model, so flat roots — see
   `browser/CLAUDE.md`), lazy directory enumeration, path-identity `NodeObject`s, arrow-key nav driven programmatically,
   selection → `BrowseSelectFolder` recorded in `browser::State` (+ supported-image count logged).
4. **Grid pane:** `NSCollectionView` wired to the thumbnail cache + `DirectoryList`, selection, empty state.
5. **Behaviors:** browse-open folder-select + scroll-to-mid + last-image select + focus; Tab; double-click/Enter →
   image; dir-arg startup; menu wiring.
6. **Styling pass:** the "beautiful gallery" look — reviewed visually with David.
7. **QA + tests + docs:** new `SharedAppState` fields (mode, selected folder, grid count, focused pane) so the QA/MCP
   server and integration tests can assert browse mode; colocated `CLAUDE.md` for the new `browser/` and `thumbnails/`
   modules; update `architecture.md` and `AGENTS.md`.

## Risks and unknowns

- **Focus/first-responder is the make-or-break** — winit's event loop and AppKit's responder chain sharing key events.
  Phase 0 proves it before anything else.
- **`NSCollectionView` styling** is craft, not a flag; baseline then iterate.
- **Grouped source-list roots** (volumes under a "Locations" header) may need an `NSOutlineView` group-row data source;
  flat roots are the fallback.
- **objc2-app-kit features to add** in `Cargo.toml`: `NSSplitView`, `NSOutlineView`, `NSCollectionView`, `NSScrollView`.

## Test plan

- Phase 1 (rename): full `./scripts/check.sh` green, no behavior change; existing preview/placeholder tests pass under
  new names.
- Phase 2 (thumbnails): headless unit tests for cache budgeting/eviction and the generate-on-demand windowing.
- Phases 3–5: integration tests via the QA/MCP server asserting mode switch, selected folder, grid count, focused pane,
  and that a dir argument boots into browse. Manual: focus/Tab/keyboard, dir-arg launch, empty folder, very large folder
  scrolling.
- Visual review with David for the styling pass and any positioning nuance.
- `./scripts/check.sh` (all checks) green before every commit. </content> </invoke>
