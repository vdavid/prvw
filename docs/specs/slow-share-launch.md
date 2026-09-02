# Slow-share launch: async folder scan, queued navigation, read progress

## Problem

Opening an image that lives in a big folder on a network share takes up to a minute before the window appears. Measured
on 2026-09-02 against a QNAP share on the macOS SMB mount (`/Volumes/naspi`):

- 7,981-file folder: `DirectoryList::from_file` (a `read_dir` plus one `canonicalize()` per file until the target is
  found) took 17.5 s on the main thread. A plain `ls` of the same folder took 44–72 s. Listing the same folder directly
  over SMB with the `smb2` crate took 1.0 s, so the NAS is fast and the macOS SMB client is the bottleneck.
- 279-file folder with ~5 MB JPEGs: launch to first frame ~2 s, of which the synchronous read+decode of the opened file
  was 1.1–1.6 s.

Everything above runs inside `App::initialize_viewer` (and the running-app `AppCommand::OpenFile` handler), before the
first redraw, so nothing paints until it all finishes. The neighbor preloads, QuickLook previews, and dimension prefetch
are already off-thread and are not the cause.

Test folders and a Finder-equivalent launch recipe with logs are in the lead's memory note; the short version:
`open --env RUST_LOG=debug --env PRVW_BACKGROUND_WINDOW=1 --stdout LOG --stderr LOG -a /Applications/Prvw.app FILE`.

## Goals

1. The window paints immediately on launch and on a running-app open, for every format and any folder size.
2. The opened image appears as soon as its own read+decode finishes, independent of the folder scan.
3. The folder scan runs off the main thread, once per folder, shared by image mode, the browse grid, and the browse
   tree.
4. Navigation requested before the scan lands is queued, applied when the scan lands, and never loses the picture.
5. Honest progress: a running count for the scan (no total exists up front), a real byte-progress bar for the image
   read.
6. Small: the per-file `canonicalize()` loop goes away, date sort stats each file once, the updater checks once per
   launch.

Out of scope: a direct-SMB listing fast path via the `smb2` crate (a later effort).

## Design

### 1. One shared folder scanner

Replace the three image/directory listers (`folder_watch::RescanLister`, `browser::grid_listing::FolderLister`, and the
tree's `TreeScanner`) with one scanner service: a single dedicated `std::thread` (no rayon, no tokio, like
`navigation::preloader`) fed by `mpsc`.

- A request names a folder. One `read_dir` pass yields both the supported image files and the child directories (use
  `DirEntry::file_type()`; it's populated from the directory read on macOS, no extra stat). The result carries both
  lists, unsorted; consumers sort with their own `SortBy`.
- Per-folder in-flight dedupe: a request for a folder that is queued or running doesn't start a second scan. If a
  request arrives for a folder whose scan is already running (a live-sync change during a long scan), flag it to re-run
  once after the current pass finishes, so a change is never missed.
- Progress: each in-flight scan owns an `Arc<AtomicUsize>` entry counter bumped per `read_dir` entry. The main thread
  can read it by folder path (a small `Mutex<HashMap<PathBuf, Arc<AtomicUsize>>>` handle, or equivalent). Also record
  the scan's start `Instant` so overlays can apply their delay.
- The result comes back as one `AppCommand` (for example `FolderScanned { folder, images, subdirs }`); the executor
  routes it to every consumer that cares about that folder: image-mode dir list, browse grid, browse tree children, and
  live sync's `apply_folder_rescan`. Consumers match by folder path.
- Test hook: an env var (for example `PRVW_SCAN_DELAY_MS`) makes the scanner sleep that long before reading, so
  integration tests can exercise the pending state deterministically. Document it next to `PRVW_BACKGROUND_WINDOW`.

### 2. Launch and open without the scan

`initialize_viewer` and the running-app `OpenFile` handler no longer read the directory.

- Install a provisional `DirectoryList` holding only the opened file (index 0 of 1) and mark navigation state as
  `scan_pending` for that folder. The multi-file launch (`explicit_files`) stays as is: no scan, no pending state.
- Title shows the filename only until the scan lands, then gains the `n/N`.
- Display the opened image through the async path for every format (today only RAW takes it): set `pending_current`,
  `prioritize_target`, request a redraw, and let `poll_preloader` display it. The "Loading…" overlay appears only if the
  image hasn't landed within 150 ms (a constant next to the tree's `LOADING_OVERLAY_DELAY`), so local files never flash
  it. Keep the RAW quick-preview behavior.
- Request the folder scan from the shared scanner, and start the live folder watch right away.
- When `FolderScanned` lands for the pending folder: build the real list with `DirectoryList::from_sorted` positioned at
  the current image by path, update the title, seed previews (`previews.set_folder`), warm neighbors
  (`warm_initial_neighbors`), then apply any queued navigation intent (section 3). If the current path isn't in the
  listing (deleted meanwhile), keep the provisional list and stay.
- Drop the per-file `canonicalize()` loop in `DirectoryList::from_file` (if it survives at all): canonicalize the parent
  once and compare paths directly.
- `sort_files` for `SortBy::Date` must stat each file once: precompute mtimes into a map, then sort by the map.

### 3. Queued navigation while the scan is pending

While `scan_pending` is set, navigation doesn't move; it records an intent and the picture stays on screen.

- Intent shape: an anchor (`Current`, `First`, `Last`) plus a signed delta. Arrow keys, wheel, and Next/Previous add to
  the delta (the debounced path can reuse `pending_nav_delta` accumulation). Home/End set the anchor to `First`/`Last`
  and reset the delta. Left then right nets to zero, which clears the intent.
- Resolution when the scan lands: anchor index plus delta; wrap when loop navigation is on, clamp to the folder edges
  when it's off (decision: clamp, same as pressing the key on a scanned folder). Then navigate through the normal path
  so preloads and previews behave as usual. A zero-delta `Current` intent is a no-op.
- Slideshow: an advance during the pending scan records the intent like a key press; the slideshow keeps its hold logic.
- QA/MCP: expose `scan_pending` (bool) and the pending intent in `/state` and `SharedAppState`, so integration tests can
  assert on them.

### 4. Status text

Light, small, gray, secondary. It's information, not a headline.

- Image mode: only while an intent is pending and the scan is in flight, show "Scanning folder… 3,412 images so far" as
  a glyphon line (same family as the "Loading…" overlay, dimmer and smaller). Nothing shows otherwise; the main screen
  doesn't advertise the scan.
- Browse grid: while the listed folder's scan is in flight, the empty grid area shows "Scanning… 3,412 images so far".
- Browse tree: the existing 1 s overlay gains the same count.
- Polling: while a scan is in flight and something on screen displays its count, wake the loop about 4 times a second
  (`ControlFlow::WaitUntil`, alongside the existing candidates in `about_to_wait`). Otherwise no polling.

### 5. Image read progress bar

- `decoding::load_image` (or its file-reading step) reads the file in chunks (256 KB) into the buffer and bumps an
  `Arc<AtomicU64>` bytes-read counter; the total is the file length from metadata. The preloader hands a progress handle
  to the main thread for the pending target.
- While `pending_current` is set and the overlay is showing (after the 150 ms delay), poll the counter about 10 times a
  second and draw a small horizontal bar under the "Loading…" text: thin gray outline, gray fill, light. Something like
  160×6 logical px. Once the read completes the bar stays full until `Ready` lands (the decode phase is indeterminate
  and short).
- The bar needs a solid-rectangle draw in the renderer. Reuse an existing quad/overlay pipeline if there is one;
  otherwise add the smallest thing that works.

### 6. Updater: one check per launch

Today a Finder launch runs `updater::check_only` while waiting for the Apple Event and `updater::check_and_update` again
in `initialize_viewer`, fetching the manifest twice. Fetch it once per process (cache the check's outcome) and have the
later call reuse it: install if the earlier check found an update, skip the network otherwise.

## Tests

- Pure unit tests: intent accumulation and resolution (clamp/wrap), scanner dedupe and re-run flag, mtime-precomputed
  date sort, provisional-list to real-list transition keeping the current image by path.
- Integration (QA server, `tests/integration.rs`): with `PRVW_SCAN_DELAY_MS` set, launch on a temp folder with a few
  hundred images, assert the image displays while `scan_pending` is true, press left then right and assert the intent
  nets to zero, press left once and assert the app lands on the previous image once the scan finishes. Follow the
  existing test style and the `PRVW_BACKGROUND_WINDOW` harness.
- TDD where it's testable: write the failing test first and see it fail.

## Docs to update

`apps/desktop/src/navigation/CLAUDE.md`, `apps/desktop/src/browser/CLAUDE.md`, `apps/desktop/src/folder_watch.rs` module
docs, `apps/desktop/CLAUDE.md`, `docs/architecture.md`, `docs/specs/live-folder-sync.md` and
`docs/specs/image-browser.md` where they describe the old listers, and `CHANGELOG.md`.
