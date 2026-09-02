# Prvw: live folder sync (mode sync + file watching)

Keep the image sequence and the browse UI correct and live: re-anchor browse mode to the current image on every entry,
and watch the filesystem so adds/modifies/deletes reflect in both modes without a manual refresh. Builds on the image
browser (`docs/specs/image-browser.md`).

Status: shipped. Both parts (mode sync, image-mode + browse-mode file watching, tree-structure watch) are built and
checks-green.

## Part 1 — Mode sync: browse always re-anchors to the current image

Browse mode re-anchors to the current image on every entry, never the stale selection from the last time you browsed: so
after you navigate in image mode and press Enter, browse shows **the image you're currently viewing**.

- **Every `enter_browse` re-anchors to `navigation`'s current image** (not the last browse state): reveal the current
  image's folder in the tree, select it, preselect that image in the grid, focus the grid, and **scroll both into view**
  (handles a selection pushed off-screen by scrolling or a window resize). Reuses the browse-open positioning, run on
  _every_ entry against the live current image, not just first entry / dir-arg.
- If there's no current image (nothing open), falls back to the last folder / home.

**How it works.** `set_view_mode(Browse)` calls `App::reveal_current_image_in_browse` on every entry; the anchor target
(the current image's parent folder + the image) is the pure `browser::browse_anchor_target`, handed to
`State::reveal_to_folder(folder, Some(image))`. The reveal walk selects the folder and the pending preselect anchors the
grid. The same-folder case (only the image within the folder changed) is handled in `outline::select_and_scroll_to`,
which dispatches `BrowseSelectFolder` itself when the target row is already selected (a plain `selectRowIndexes:`
wouldn't fire the selection-changed delegate). See `src/browser/CLAUDE.md` → "Every browse entry re-anchors to the live
current image".

## Part 2 — File watching (both modes)

A live filesystem watcher keeps the data correct. Two roles, ideally one shared watcher manager:

- **Active-folder image watch:** the folder whose images are currently shown — image mode → the current image's folder;
  browse mode → the grid's listed folder (the same folder once synced). Re-target when the active folder changes.
- **Tree-structure watch:** the currently-expanded tree folders (roots always; plus folders the user expands). Bounded
  to what's visible — we do NOT watch the whole disk. Watches are added on expand and removed on collapse.

### Mechanism

- **`notify` crate** (wraps macOS FSEvents). The `folder_watch` module owns a `notify` watcher over a **dynamic set of
  non-recursive paths**, exposing `watch(path)` / `unwatch(path)`.
- Events run off the main thread; **coalesce/debounce ~150ms** (bursts, and editors' temp-write-rename saves) and post
  to the main thread via the `EventLoopProxy` as an `AppCommand` carrying the changed folder (and changed paths/kinds).
  Never block the main thread.
- The main-thread handler routes by the changed path: active image folder → image-list update; an expanded tree folder →
  tree-children update.

### Browse-mode live sync

The active-folder (image-list) watch **follows the grid's listed folder in browse, the current image's folder in image
mode** (`App::active_folder`); `retarget_active_folder_watch` re-targets on every mode switch, browse folder listing (a
`FolderScanned` listing the grid's folder), and image open. They coincide once synced, but a user can browse a different
folder than the open image — we watch what's shown.

A `FolderChanged` for the grid's listed folder re-scans off-thread (the shared `folder_scan::FolderScanner`), and
`apply_folder_rescan` updates BOTH the grid (`State::apply_grid_rescan` → `BrowseGrid::apply_rescan`) and the image-mode
`dir_list` when each owns the folder, so synced modes stay coherent. The grid: inserts adds at the sorted position,
drops removes, **keeps the selection by path** (pure `grid_model::select_after_rescan` — next/previous surviving image
when the selected file is deleted, empty when the folder emptied), and refreshes thumbnails (a full clear-and-repump
regenerates modified/added cells from the shared QuickLook cache). A changed selection re-warms via
`warm_browse_selection`.

**Tree-structure watch lifecycle (Part B).** Roots are watched for the window's life (`watch_tree_roots` at browse setup
/ dir-arg launch). Each expanded tree folder is watched on `outlineViewItemDidExpand:` (→ `BrowseTreeFolderExpanded` →
`App::watch_tree_folder`) and unwatched on `outlineViewItemDidCollapse:` (→ `BrowseTreeFolderCollapsed` →
`App::unwatch_tree_folder`). Collapse never unwatches a **root**, nor a folder still serving as the active image-list
folder. Bounded: only roots + expanded folders are watched, never the whole disk. A `FolderChanged` for a watched tree
folder re-scans its subdirectories (`State::reload_tree_node` → invalidate the child cache + re-scan via the existing
shared folder scanner). The completion is **subdir-delta-gated** — `reloadItem:reloadChildren:` only if a subfolder was
actually added/removed (a busy ancestor's file churn must not reload the tree) — and **selection-preserving** — it
restores a surviving folder's selection without re-listing the grid, and the deleted-selected fallback (select the
reloaded parent) fires only when the folder is genuinely gone from disk. See "Tree-structure updates" below.

**Routing.** One `FolderChanged { folder }` can match two roles, and BOTH fire (neither is `else` to the other): the
**active image-list folder** (→ grid + `dir_list` update) AND/OR a **watched tree node** (→ subdir reload). A folder can
be both the listed folder and an expanded tree node.

### Image-list updates (active folder, both modes)

Driven by re-scanning the folder (robust against rename-saves) plus per-path `Modify` events:

- **Added image:** insert at the correct position for the active `SortBy`. Image mode: recalc the current index (keep
  pointing at the same current image _by path_; the existing re-sort-by-path logic in `DirectoryList` covers this).
  Browse mode: insert the grid item (and schedule its thumbnail).
- **Modified image:** reload it seamlessly in both modes — evict the path from `image_cache` and drop its
  preview/thumbnail so it regenerates; if it's the currently displayed image, re-decode and repaint; if it's a visible
  grid cell, refresh its thumbnail.
- **Deleted image:** remove from the sequence and the grid. If it's **not** the current image, just drop it (recalc
  indices). If it **is** the current image: navigate to the **next** available image, or the **previous** if it was the
  last; if it was the **only** image in the folder, show a graceful **"(No images)" empty state in image mode** (black
  canvas + centered overlay — browse mode already has one). Evict the deleted path from all caches.

### Tree-structure updates (expanded folders)

The tree shows **directories only**, so it reacts to sub-folder changes, never file changes. Both rules below matter
because a reveal expands and watches the target's ancestors, and a busy ancestor (`/tmp`, Downloads — every process
writes temp files there) fires file-change events constantly:

- **Subdir-delta gating.** On a change in a watched (expanded) folder, re-scan its subdirectories (the existing async
  scan) and compare the fresh subdir set to what the node showed. `reloadItem:reloadChildren:` **only if the subdir set
  actually changed** (a subfolder was added or removed). If only files changed, do nothing to the tree. This stops a
  busy ancestor from churning the tree. (The active-folder _image_ watch still handles file changes for the
  grid/sequence — separate, and unaffected.)
- **A reload never corrupts the selection.** `reloadItem:reloadChildren:` can drop the selected descendant's selection
  even when it still exists. Preserve it: re-select the same folder if it still exists (without re-listing the grid —
  the user never re-selected it), preserving expansion of surviving descendants. The **"deleted selected folder → select
  the parent" fallback fires ONLY when the selected folder is genuinely gone from disk**, never during a routine reload
  where it still exists. A tree-node reload never dispatches `BrowseSelectFolder` / re-lists the grid for the reloaded
  node itself — only a real user selection (or the intentional reveal) changes the selected folder + grid.
- Watch lifecycle: add a watch when a node expands (after its children load), remove it on collapse. Roots stay watched.

The gating + fallback decision are pure and unit-tested (`subdirs_changed`, `selection_action_after_reload` in
`outline.rs`); the objc2 reload/re-select is covered by smoke + live QA.

### Cache discipline

`image_cache` (LRU by path), the QuickLook preview cache, and the grid thumbnail cache must all evict/refresh the
affected path on modify/delete so nothing stale survives. A regenerated thumbnail must not be served from a stale
QuickLook entry (QuickLook keys on file content/mtime, so a fresh request regenerates; force-evict our own caches).

## Component map

- **Watcher infrastructure.** The `folder_watch` module: a `notify` FSEvents watcher over a dynamic non-recursive path
  set (`watch`/`unwatch`) and a pure debounce/coalescer (`Coalescer`, ~150 ms), posting `AppCommand::FolderChanged` via
  the `EventLoopProxy`. The re-read it triggers rides `crate::folder_scan`, which answers with
  `AppCommand::FolderScanned`. Headless-tested: the coalescer and the folder-diff (`navigation::folder_diff`, old list
  vs rescanned list → add/remove + delete-current outcome under each `SortBy`).
- **Image-mode live sync.** The active-folder watch drives sequence updates, cache + preview eviction, current-by-path
  recalc, delete-current navigation (next / previous / empty), and the image-mode "(No images)" empty state. See
  `navigation/CLAUDE.md` → "Live folder sync (image mode)".
- **Browse-mode live sync.** The active-folder watch follows the grid's listed folder in browse; `apply_folder_rescan`
  updates the grid (add/remove/modify + selection-by-path + empty state) alongside image mode. The tree-structure watch
  (roots + expanded folders, expand/collapse lifecycle) reloads a watched node's subdirectories on change, preserving
  expansion/selection. See "Browse-mode live sync" above and `src/browser/CLAUDE.md` → "Live folder sync (browse mode)".

## Risks / notes

- **Watch-lifecycle on expand/collapse** is the fiddliest part — the subdir-delta gate + selection-preserving reload
  (see "Tree-structure updates") are what keep it correct under a busy ancestor.
- **Event storms** (bulk copy/delete): debounce + full re-scan of the affected folder keeps state correct without
  per-event thrash.
- **Atomic saves** (temp + rename) look like Create/Remove/Rename; re-scan-on-debounce reflects the final state.
- **Never block the main thread** — re-scans of a slow (e.g. SMB) folder run on the existing background scanners, not
  inline in the event handler.

## Test plan

- Headless unit tests: the folder-diff (add/remove/reorder under each `SortBy`), the debounce/coalesce, the
  delete-current → next/prev/empty decision.
- Integration tests (QA-driven, temp folder): add an image → appears in sequence + grid at the right spot; modify →
  reloads; delete a non-current → vanishes; delete the current → navigates to next/prev; delete the last → image-mode
  "(No images)"; re-enter browse after navigating → current image selected + scrolled into view.
- Manual: live add/modify/delete in Finder while viewing/browsing; SMB folder (no main-thread stall); rapid bulk
  changes.
- `./scripts/check.sh` (all checks) green before every commit. </content>
