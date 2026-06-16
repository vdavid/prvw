# Prvw: live folder sync (mode sync + file watching)

Keep the image sequence and the browse UI correct and live: re-anchor browse mode to the current image on every entry,
and watch the filesystem so adds/modifies/deletes reflect in both modes without a manual refresh. Builds on the image
browser (`docs/specs/image-browser.md`).

Status: spec / in progress. Same `image-browser` worktree + branch.

## Part 1 — Mode sync: browse always re-anchors to the current image — DONE

Re-entering browse mode used to restore the _stale_ selection from the last time you were in browse. Now: after you
navigate in image mode and press Enter, browse shows **the image you're currently viewing**.

- **Every `enter_browse` re-anchors to `navigation`'s current image** (not the last browse state): reveal the current
  image's folder in the tree, select it, preselect that image in the grid, focus the grid, and **scroll both into view**
  (handles a selection pushed off-screen by scrolling or a window resize). Reuses the Phase-5 browse-open positioning,
  run on _every_ entry against the live current image, not just first entry / dir-arg.
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

- **`notify` crate** (wraps macOS FSEvents). Pin a stable version ≥14 days old; verify license compatibility before
  adding. A `folder_watch` module owns a `notify` watcher over a **dynamic set of non-recursive paths**, exposing
  `watch(path)` / `unwatch(path)`.
- Events run off the main thread; **coalesce/debounce ~150ms** (bursts, and editors' temp-write-rename saves) and post
  to the main thread via the `EventLoopProxy` as an `AppCommand` carrying the changed folder (and changed paths/kinds).
  Never block the main thread.
- The main-thread handler routes by the changed path: active image folder → image-list update; an expanded tree folder →
  tree-children update.

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

- On a change in a watched (expanded) folder, re-scan that folder's subdirectories (the existing async scan) and
  `reloadItem:reloadChildren:` that node. Preserve expansion/selection where possible. New subfolders appear, deleted
  ones vanish.
- Watch lifecycle: add a watch when a node expands (after its children load), remove it on collapse. Roots stay watched.
  If a currently-selected/revealed folder is deleted, degrade gracefully (select its parent or a root).

### Cache discipline

`image_cache` (LRU by path), the QuickLook preview cache, and the grid thumbnail cache must all evict/refresh the
affected path on modify/delete so nothing stale survives. A regenerated thumbnail must not be served from a stale
QuickLook entry (QuickLook keys on file content/mtime, so a fresh request regenerates; force-evict our own caches).

## Build order

1. **Mode sync** (Part 1) — small, independent; fixes the reported re-entry bug. Reuses Phase-5 positioning on every
   entry.
2. **Watcher infrastructure** — DONE. The `folder_watch` module: a `notify` FSEvents watcher over a dynamic
   non-recursive path set (`watch`/`unwatch`), a pure debounce/coalescer (`Coalescer`, ~150 ms), and an off-thread
   `RescanLister`, posting `AppCommand::FolderChanged` / `ActiveFolderRescanned` via the `EventLoopProxy`.
   Headless-tested: the coalescer and the folder-diff (`navigation::folder_diff`, old list vs rescanned list →
   add/remove + delete-current outcome under each `SortBy`).
3. **Image-mode live sync** — DONE. The active-folder watch is wired to sequence updates, cache + preview eviction,
   current-by-path recalc, delete-current navigation (next / previous / empty), and the image-mode "(No images)" empty
   state. See `navigation/CLAUDE.md` → "Live folder sync (image mode)".
4. **Browse-mode live sync** — grid list + thumbnail refresh on change; tree expanded-folder watch → subfolder updates.
   The active-folder watch infra (steps 2–3) is built to be shared by browse; only image mode is wired so far.
5. **Tests + docs** — headless tests for the diff/debounce + integration tests (add/modify/delete in a temp folder, both
   modes, via the QA hooks); update `browser/CLAUDE.md`, `navigation/CLAUDE.md`, `architecture.md`, this spec.

## Risks / notes

- **Watch-lifecycle on expand/collapse** is the fiddliest part; if it gets gnarly, land steps 1–3 (mode sync + image
  list) first and treat the tree-structure watch as an additive follow-up.
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
