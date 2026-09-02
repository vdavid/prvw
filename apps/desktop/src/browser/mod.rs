//! # Browser (browse mode)
//!
//! A second top-level screen for the main window: a native AppKit `NSSplitView` (folder tree
//! on the left, thumbnail grid on the right) that **swaps** with the wgpu image viewer. Image
//! mode is the GPU surface; browse mode is fully native AppKit. They never overlap — a
//! transparent Metal pixel still occludes in-window content behind it, so we hide one and show
//! the other (see the "Native AppKit views over/around the wgpu Metal layer" gotcha in
//! `platform/macos/CLAUDE.md`).
//!
//! See `docs/specs/image-browser.md` for the full design and `browser/CLAUDE.md` for the module
//! map.
//!
//! ## Architecture at a glance
//!
//! - The split view is a **sibling subview of winit's contentView** (same pattern as
//!   `window::add_titlebar_labels`): `addSubview`, layer-backed, `zPosition` above the Metal
//!   layer's `1.0`, pinned to the contentView edges, with a stable `identifier` for hide/show.
//! - **Render from state.** `browser::State` is the single source of truth (`mode`, `focused_pane`,
//!   selected folder, grid selection). One idempotent `sync_native` reads it and sets ALL derived
//!   native UI — split-view + Metal-layer visibility, the image title/zoom labels, the first
//!   responder, the grid-selection anchor, and emphasis. Every mutation funnels through it
//!   (mutate state → `sync_native`); nothing pokes native views ad-hoc, so the native UI can't
//!   drift from state. See `CLAUDE.md` → "Browse UI architecture".
//! - Keyboard: in idle-winit browse mode the focused native view holds first responder and
//!   handles its own arrows natively. The focused view's `keyDown:` override handles only
//!   Tab/Enter/Esc (routed via `AppCommand`); everything else falls through to `super` for native
//!   selection/scroll. See `docs/specs/image-browser.md`.

// Browse mode has a native UI on macOS and Windows, and the pure logic here (`grid_model`,
// `tree_model`, the pane/reveal/warm helpers below) is consumed only by that native code. So on a
// platform with neither, every one of these is unused. Keep them compiled everywhere — their unit
// tests run on every host — but silence the warnings that build would otherwise raise under
// `-D warnings`. Deliberately NOT silenced on Windows: dead code in `browser::windows` is
// something to hear about rather than hide.
#![cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]

#[cfg(target_os = "macos")]
mod grid;
pub(crate) mod grid_model;
#[cfg(target_os = "macos")]
mod outline;
#[cfg(target_os = "macos")]
mod split_view;
pub(crate) mod tree_model;

// Browse mode's Windows shell. Only the `ui` submodule inside it is Windows-gated; everything
// else compiles here so a Mac's test run asserts what the Windows browser will show.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub mod windows;

// The grid's headless plumbing (Phase 2): the visible-range scheduler and the byte-budget
// eviction state. Both pure and unit-tested; the `NSCollectionView` grid (`grid`) drives them.
// `#[allow(dead_code)]` covers the slice of their API the grid doesn't call yet — `Scheduler`'s
// `pause`/`resume`/`status`/`visible_range` and the `ThumbnailCache` inspectors are for the
// browse-behaviors + QA phases (the scheduler pause mirrors previews'; `status` feeds the MCP
// snapshot). The methods the grid drives today are exercised; these round out the API.
#[allow(dead_code)]
pub mod grid_scheduler;
#[allow(dead_code)]
pub mod thumbnail_cache;

/// The two top-level screens of the main window. The viewer starts in `Image`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// The wgpu image viewer (today's default screen).
    Image,
    /// The native AppKit browse screen (folder tree + thumbnail grid).
    Browse,
}

impl ViewMode {
    /// The mode you land in after toggling. Image ↔ Browse.
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            ViewMode::Image => ViewMode::Browse,
            ViewMode::Browse => ViewMode::Image,
        }
    }

    /// True when this is browse mode.
    #[must_use]
    pub fn is_browse(self) -> bool {
        matches!(self, ViewMode::Browse)
    }
}

/// Which of the two browse panes has keyboard focus. Tab flips it. This is the single source of
/// truth for "which pane is focused" (`browser::State::focused_pane`, an `Option` — `None` in
/// image mode). `apply_focus` syncs the native first responder to it; nothing infers focus from
/// the native first responder. See `docs/specs/image-browser.md` → "Input architecture".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSide {
    /// The folder tree (left pane).
    Tree,
    /// The thumbnail grid (right pane).
    Grid,
}

impl PaneSide {
    /// The other pane. Tab toggles between the two.
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            PaneSide::Tree => PaneSide::Grid,
            PaneSide::Grid => PaneSide::Tree,
        }
    }
}

// macOS hardware key codes (carbon `kVK_*`) the browse panes' `keyDown:` overrides intercept.
// Everything else falls through to `super` for native selection/scroll/type-select.
#[cfg(target_os = "macos")]
mod key_codes {
    /// `kVK_Return` — the main Return key.
    pub const RETURN: u16 = 36;
    /// `kVK_Tab`.
    pub const TAB: u16 = 48;
    /// `kVK_Escape`.
    pub const ESCAPE: u16 = 53;
    /// `kVK_ANSI_KeypadEnter` — the numeric-keypad Enter.
    pub const KEYPAD_ENTER: u16 = 76;
}

/// Map a hardware key code from a focused browse pane's `keyDown:` to the `AppCommand` it should
/// route, or `None` to let the native view handle it (arrows, page keys, type-select, …). The same
/// mapping serves both panes: Enter → `BrowseOpenSelected` (the executor opens the selected grid
/// image when the grid is focused, else returns to image mode), Esc → `EnterImageMode`, Tab →
/// `ToggleBrowseFocus`. Pure (headless-tested).
#[cfg(target_os = "macos")]
#[must_use]
pub fn browse_keydown_command(key_code: u16) -> Option<crate::commands::AppCommand> {
    match key_code {
        key_codes::TAB => Some(crate::commands::AppCommand::ToggleBrowseFocus),
        key_codes::ESCAPE => Some(crate::commands::AppCommand::EnterImageMode),
        key_codes::RETURN | key_codes::KEYPAD_ENTER => {
            Some(crate::commands::AppCommand::BrowseOpenSelected)
        }
        _ => None,
    }
}

/// The pane Tab should move to from `current`, given whether the grid is empty. Tab toggles
/// Tree ⇄ Grid, but an empty grid is non-focusable, so a Tab toward an empty grid stays put.
/// Pure (headless-tested); `toggle_focus` drives the native side from the result.
#[must_use]
pub fn next_focused_pane(current: PaneSide, grid_empty: bool) -> PaneSide {
    let target = current.toggled();
    if matches!(target, PaneSide::Grid) && grid_empty {
        current
    } else {
        target
    }
}

/// Where browse mode should land focus on entry: the grid when it has images (so the gallery is
/// immediately keyboard-navigable), else the tree (an empty grid is non-focusable). Pure
/// (headless-tested); `enter_browse` drives the native side from the result.
#[must_use]
pub fn browse_entry_pane(grid_empty: bool) -> PaneSide {
    if grid_empty {
        PaneSide::Tree
    } else {
        PaneSide::Grid
    }
}

/// The grid indices to warm when the browse selection lands on `selected`: the selection itself
/// (first, the prospective current image) plus `radius` neighbors each side, clamped to
/// `[0, total)`. Browse never loops (loop navigation is an image-mode concept), so the window stops
/// at the folder edges. Returns empty when the folder is empty. Pure (headless-tested); the executor
/// maps these to paths and hands them to `Preloader::warm_paths`.
///
/// Callers pass `preloader::preload_count()`. Warming lands in the same `ImageCache` the image-mode
/// preloader fills, so warming wider than that cache retains would evict on arrival and leave
/// browse slower than not warming at all. Taking the radius as a parameter is what keeps this
/// function pure and its tests independent of the host's RAM.
#[must_use]
pub fn browse_warm_indices(selected: usize, total: usize, radius: usize) -> Vec<usize> {
    if total == 0 || selected >= total {
        return Vec::new();
    }
    crate::navigation::wrap::active_preload_indices(
        selected, total, radius, /* loop_on */ false,
    )
}

/// The `DirectoryList` index a reveal lands on for the grid's `selected` index, given the grid's
/// image list and the active sort. This is the load-bearing invariant behind "the browse selection
/// IS the image-mode current image": `reveal_selected_image` rebuilds navigation with
/// `DirectoryList::from_explicit(images, sort)` (which re-sorts) then `go_by(index)`. The grid
/// already lists in the same `SortBy`, so re-sorting is idempotent and the grid's selected index
/// maps 1:1 to the dir-list index — `Some(selected)` when in range, `None` for an empty list or an
/// out-of-range index (graceful no-selection: the reveal keeps the current image). Pure
/// (headless-tested); the windowed reveal reads the resolved index back from the rebuilt list.
#[must_use]
pub fn resolve_reveal_index(
    images: &[std::path::PathBuf],
    selected: usize,
    sort_by: crate::navigation::SortBy,
) -> Option<usize> {
    if images.is_empty() || selected >= images.len() {
        return None;
    }
    // The grid hands its images already sorted by `sort_by`; `from_explicit` re-sorts with the
    // same comparator, so the path at `selected` keeps its position. Resolve by path to stay
    // correct even if a caller ever passes an unsorted list.
    let mut sorted = images.to_vec();
    crate::navigation::sort::sort_files(&mut sorted, sort_by);
    let target = images.get(selected)?;
    sorted.iter().position(|p| p == target)
}

/// The grid index to preselect when revealing a folder that contains `current_image`, given the
/// grid's image list (already sorted in the grid's `SortBy`). Returns the position of
/// `current_image` in `images`, or `None` when the image isn't in the list (a different folder, or
/// no current image). The caller falls back to index 0 (preselect the first image) on `None`.
///
/// This is the browse-open round-trip invariant: entering browse from an image preselects that
/// exact image in the grid, so Esc/Enter right after open reveals the same image you came from
/// (the grid selection drives the image-mode current via `resolve_reveal_index`). Pure
/// (headless-tested), and no disk access: both sides are concrete paths already.
///
/// **Matched under the host's path policy, never with `==`.** The two sides reach here spelled
/// differently on Windows: the displayed image carries the canonical `\\?\C:\…` its launch
/// produced, while the grid's images come from a listing of the folder the browse tree selected,
/// which is spelled the way a drive enumeration and `read_dir` produced it. A byte comparison
/// finds nothing and the grid quietly preselects the first image instead of the one you came from.
#[must_use]
pub fn grid_preselect_index(
    images: &[std::path::PathBuf],
    current_image: Option<&std::path::Path>,
) -> Option<usize> {
    grid_preselect_index_under(crate::paths::PathPolicy::HOST, images, current_image)
}

/// [`grid_preselect_index`] under a named path policy, so a Mac can assert what Windows will do.
#[must_use]
pub fn grid_preselect_index_under(
    policy: crate::paths::PathPolicy,
    images: &[std::path::PathBuf],
    current_image: Option<&std::path::Path>,
) -> Option<usize> {
    let current = current_image?;
    images.iter().position(|p| policy.same_path(p, current))
}

/// Is the folder listing that just landed the one a browse-open reveal was waiting for?
///
/// The browse-open state (`pending_grid_preselect`, `pending_browse_open_focus`) is one-shot, and
/// it belongs to the folder the reveal walk is heading for. Any other listing that lands first
/// would spend it: on Windows a treeview picks its own first row the moment it takes focus, which
/// listed the drive root before the walk had gone anywhere, and the grid then neither preselected
/// the came-from image nor took the focus. `None` (no reveal in flight) matches everything, which
/// is a plain folder click and behaves as it always has. Pure, so a Mac asserts the Windows
/// spellings. See [`crate::paths`] for why this can't be `==`.
#[must_use]
pub fn reveal_landing_matches(
    policy: crate::paths::PathPolicy,
    pending_reveal: Option<&std::path::Path>,
    listed: &std::path::Path,
) -> bool {
    pending_reveal.is_none_or(|target| policy.same_path(target, listed))
}

/// The browse-entry anchor target for a current image: the folder to reveal/select (the image's
/// parent) plus the image itself to preselect in the grid. `None` when there's no current image
/// (nothing open) or the path has no parent — the caller then falls back to today's behavior (the
/// last browse state / home). This is the load-bearing rule behind "every browse entry re-anchors
/// to the live current image": the caller hands the result to `State::reveal_to_folder(folder,
/// Some(image))`, and `grid_preselect_index` then maps the image to its grid slot. Pure
/// (headless-tested); the objc2 reveal/scroll is covered by the smoke run + live QA.
#[must_use]
pub fn browse_anchor_target(
    current_image: Option<&std::path::Path>,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let image = current_image?;
    let folder = image.parent()?;
    Some((folder.to_path_buf(), image.to_path_buf()))
}

/// What a launch argument resolves to, deciding the startup screen. Computed purely from a path's
/// kind (`is_file` / `is_dir`) so the dir-vs-file branch is unit-testable without spinning up the
/// app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchTarget {
    /// A readable image file → image mode (today's behavior).
    Image,
    /// A readable directory → browse mode, revealed + selected in the tree.
    Directory,
    /// Neither a readable file nor directory (missing, unreadable, or an unsupported kind) →
    /// onboarding, exactly as a no-argument launch.
    Onboarding,
}

/// Classify a launch path into the startup screen. `is_file`/`is_dir` are passed in (rather than
/// hitting the disk here) so the decision is pure and testable; the caller reads them from
/// `Path::is_file()` / `Path::is_dir()` once. A file wins over a directory if a path somehow
/// reports both (it can't in practice). Pure (headless-tested).
#[must_use]
pub fn classify_launch_target(is_file: bool, is_dir: bool) -> LaunchTarget {
    if is_file {
        LaunchTarget::Image
    } else if is_dir {
        LaunchTarget::Directory
    } else {
        LaunchTarget::Onboarding
    }
}

/// The winit window's `HWND`, or `None` before it exists.
#[cfg(target_os = "windows")]
fn window_hwnd(window: &winit::window::Window) -> Option<::windows::Win32::Foundation::HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // `::windows` rather than `windows`: this module has a submodule by that name, and the
    // crate is the one meant here.
    let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    Some(::windows::Win32::Foundation::HWND(
        handle.hwnd.get() as *mut std::ffi::c_void
    ))
}

/// Per-feature browse-mode state (sibling of `zoom::State`, `navigation::State`, …). Holds the
/// current `ViewMode` and, on macOS, the native split-view handles built lazily on first entry.
pub struct State {
    /// Current top-level screen. Starts in `Image`.
    mode: ViewMode,
    /// The single source of truth for which pane is focused: `None` in image mode, `Some(Tree)` /
    /// `Some(Grid)` in browse mode. `sync_native` derives the native first responder + emphasis
    /// from it. Never inferred from the native first responder.
    focused_pane: Option<PaneSide>,
    /// The folder currently selected in the tree. Set by `BrowseSelectFolder`; the grid lists its
    /// images. `None` until the user picks a folder.
    selected_folder: Option<std::path::PathBuf>,
    /// The grid's selected image index within `selected_folder`, mirrored here for QA/tests. The
    /// authoritative selection lives in the grid model; this tracks it. `None` when the grid is
    /// empty / nothing picked.
    grid_selected: Option<usize>,
    /// The sort order the grid lists folder images in. Read from settings at startup so the grid
    /// and image-mode `DirectoryList` agree (opening a grid item lands on the matching index).
    sort_by: crate::navigation::SortBy,
    /// The image to preselect (focused/blue) when the next folder listing lands — browse-open
    /// positioning: the image the user came from, so Esc/Enter right after opening reveals the same
    /// image. Set by `reveal_to_folder`, consumed by `grid_folder_listed`. `None` when there's no
    /// came-from image (a dir-arg launch), in which case the grid preselects index 0.
    pending_grid_preselect: Option<std::path::PathBuf>,
    /// True when the next folder listing is a browse-open reveal (entering browse from an image, or
    /// a dir-arg launch) — so the grid should take focus once its images land (the reveal's tree
    /// selection had focused the tree). Set by `reveal_to_folder`, consumed by `grid_folder_listed`.
    /// Separate from `pending_grid_preselect` because a dir-arg launch focuses the grid yet has no
    /// came-from image to preselect.
    pending_browse_open_focus: bool,
    /// The folder a browse-open reveal is walking towards, while one is in flight. It gates the
    /// two `pending_*` fields above: a listing for any other folder must not spend them. See
    /// [`reveal_landing_matches`].
    pending_reveal_folder: Option<std::path::PathBuf>,
    /// The native split view + its panes, built once on first entry to browse mode and kept
    /// alive for the window's lifetime thereafter. `None` until first built.
    #[cfg(target_os = "macos")]
    split_view: Option<split_view::BrowseSplitView>,
    /// The native Win32 browser — a container child window holding the tree, the grid, and the
    /// status bar. Built once on first entry to browse mode and kept for the window's lifetime,
    /// exactly as the macOS split view is. `None` until first built.
    #[cfg(target_os = "windows")]
    browse_ui: Option<windows::BrowseUi>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: ViewMode::Image,
            focused_pane: None,
            selected_folder: None,
            grid_selected: None,
            sort_by: crate::navigation::SortBy::default(),
            pending_grid_preselect: None,
            pending_browse_open_focus: false,
            pending_reveal_folder: None,
            #[cfg(target_os = "macos")]
            split_view: None,
            #[cfg(target_os = "windows")]
            browse_ui: None,
        }
    }

    /// Set the sort order the grid lists folder images in. Called at startup from the persisted
    /// setting so the grid and image-mode `DirectoryList` use the same ordering.
    pub fn set_sort_by(&mut self, sort_by: crate::navigation::SortBy) {
        self.sort_by = sort_by;
    }

    /// The grid's selected image index, mirrored from the grid model. `None` when empty.
    #[must_use]
    pub fn grid_selected(&self) -> Option<usize> {
        self.grid_selected
    }

    /// How many images the grid currently lists (the selected folder's supported-image count). `0`
    /// when the grid is empty or the split view isn't built. Exposed in the QA snapshot so tests can
    /// assert a folder listing landed without a screenshot. Off macOS the grid doesn't exist → `0`.
    #[cfg(not(target_os = "windows"))]
    #[must_use]
    pub fn grid_count(&self) -> usize {
        #[cfg(target_os = "macos")]
        {
            self.split_view
                .as_ref()
                .map_or(0, |s| s.grid().images().len())
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// True while the tree's async reveal walk is in flight (browse-open positioning expands
    /// ancestors toward the target folder). `false` when idle or the split view isn't built. Lets
    /// QA/tests poll for the reveal to settle before asserting the landed folder/grid. Off macOS
    /// there's no native tree → always `false`.
    #[cfg(not(target_os = "windows"))]
    #[must_use]
    pub fn reveal_pending(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.split_view
                .as_ref()
                .is_some_and(split_view::BrowseSplitView::reveal_pending)
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// The folder currently selected in the browse-mode tree, if any.
    #[must_use]
    pub fn selected_folder(&self) -> Option<&std::path::Path> {
        self.selected_folder.as_deref()
    }

    /// Record the folder selected in the tree. Listing its images for the grid is the executor's
    /// job (it asks the shared `folder_scan::FolderScanner` and applies the result via
    /// [`grid_folder_listed`](Self::grid_folder_listed)) — nothing here reads the disk, because a
    /// slow folder selection must not freeze the UI.
    pub fn set_selected_folder(&mut self, folder: std::path::PathBuf) {
        self.selected_folder = Some(folder);
        // Nothing here reads the disk: the executor asks the shared scanner, and this only tells
        // the Windows status bar to say "Loading…" until the answer arrives.
        #[cfg(target_os = "windows")]
        if self.browse_ui.is_some() {
            windows::listing_started();
        }
    }

    /// Take the one-shot browse-open state — the image to preselect and whether the grid should
    /// take focus — for a listing of `folder`, or leave it alone when this listing isn't the
    /// reveal's. See [`reveal_landing_matches`] for why a listing can arrive for the wrong folder.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn take_browse_open_state(
        &mut self,
        folder: &std::path::Path,
    ) -> (Option<std::path::PathBuf>, bool) {
        if !reveal_landing_matches(
            crate::paths::PathPolicy::HOST,
            self.pending_reveal_folder.as_deref(),
            folder,
        ) {
            log::debug!(
                "Browse: {} listed while revealing elsewhere — keeping the browse-open state",
                crate::paths::for_display(folder)
            );
            return (None, false);
        }
        self.pending_reveal_folder = None;
        (
            self.pending_grid_preselect.take(),
            std::mem::take(&mut self.pending_browse_open_focus),
        )
    }

    /// Apply a completed background folder listing to the grid: populate the model, reload the
    /// collection view, toggle the empty overlay, and start thumbnail generation. Updates the
    /// tracked grid selection, then `sync_native`s — the listing changes the grid's
    /// emptiness/contents, so focus + the grid-selection invariant may need re-deriving (for
    /// example, an empty→non-empty listing must give the now-focusable grid an anchor). No-op if the
    /// split view isn't built.
    #[cfg(target_os = "macos")]
    pub fn grid_folder_listed(
        &mut self,
        folder: &std::path::Path,
        images: Vec<std::path::PathBuf>,
        window: &winit::window::Window,
    ) {
        let (preselect, was_browse_open) = self.take_browse_open_state(folder);
        if let Some(split) = &self.split_view {
            split.grid().folder_listed(images, preselect.as_deref());
            self.grid_selected = split.grid().selected_index();
        }
        // Browse-open positioning focuses the GRID once its images land (the reveal walk's tree
        // selection had focused the tree, since the grid was empty when `enter_browse` ran). A
        // plain folder click keeps the tree focused. Only move focus when the grid actually has
        // images (an empty revealed folder stays on the tree — the grid is non-focusable).
        if was_browse_open && !self.grid_is_empty() {
            self.focused_pane = Some(PaneSide::Grid);
        }
        self.sync_native(window);
    }

    /// Reveal `folder` in the tree (expand from its root, select + scroll-to-mid — async) and, when
    /// that folder's images list, preselect `current_image` in the grid. This is browse-open
    /// positioning: entering browse from an image opens already showing where you are. The tree
    /// selection that ends the reveal walk fires `BrowseSelectFolder`, which lists the folder; the
    /// stored `pending_grid_preselect` then drives the grid's preselection (so Esc/Enter right
    /// after open round-trips to the same image). No-op off macOS or if the split view isn't built.
    #[cfg(target_os = "macos")]
    pub fn reveal_to_folder(
        &mut self,
        folder: &std::path::Path,
        current_image: Option<std::path::PathBuf>,
    ) {
        self.pending_grid_preselect = current_image;
        self.pending_browse_open_focus = true;
        self.pending_reveal_folder = Some(folder.to_path_buf());
        if let Some(split) = &self.split_view {
            split.reveal_folder_in_tree(folder);
        }
    }

    /// Apply a live folder re-scan to the grid (browse-mode live sync): update the listed images
    /// preserving the selection by path, refresh thumbnails for the change, and re-sync the native
    /// UI (the grid may have flipped empty↔non-empty, so focus + the grid-selection anchor may need
    /// re-deriving). Tracks the grid's selection mirror. Returns `true` when the selected image
    /// changed identity (added/removed shifted it, or the selected file was deleted), so the
    /// executor can re-warm the new selection. No-op (returns `false`) if the split view isn't
    /// built.
    #[cfg(target_os = "macos")]
    pub fn apply_grid_rescan(
        &mut self,
        images: Vec<std::path::PathBuf>,
        modified: &[std::path::PathBuf],
        window: &winit::window::Window,
    ) -> bool {
        let selection_changed = if let Some(split) = &self.split_view {
            let changed = split.grid().apply_rescan(images, modified);
            self.grid_selected = split.grid().selected_index();
            changed
        } else {
            false
        };
        self.sync_native(window);
        selection_changed
    }

    /// Drain queued grid-thumbnail completions into the collection view's cells. No-op if the split
    /// view isn't built.
    #[cfg(target_os = "macos")]
    pub fn grid_thumbnails_available(&self, mtm: objc2::MainThreadMarker) {
        if let Some(split) = &self.split_view {
            split.grid().thumbnails_available(mtm);
        }
    }

    /// Feed the grid's current visible range to its scheduler/cache and pump generation. Called on
    /// scroll. No-op if the split view isn't built or the grid is empty. macOS only: the Windows
    /// listview says when it needs items (`LVN_ODCACHEHINT`), so nothing has to poll it.
    #[cfg(target_os = "macos")]
    pub fn grid_pump_visible_range(&self) {
        if let Some(split) = &self.split_view {
            split.grid().pump_visible_range();
        }
    }

    /// Record a grid selection (native click or programmatic) and move keyboard focus to the grid
    /// pane, then render — a click in the grid focuses it, so a following Enter / double-click opens
    /// that image. Mutates state (the single source of truth) then `sync_native`s the result.
    #[cfg(target_os = "macos")]
    pub fn set_grid_selected(&mut self, index: usize, window: &winit::window::Window) {
        self.focus_grid_state(index);
        self.sync_native(window);
    }

    /// Select grid item `index` the way a native click would: update the grid model's selection
    /// (so the open path reads the right image) AND record it in state + focus the grid. The native
    /// `didSelectItemsAtIndexPaths:` delegate does both halves; this drives the same effect for the
    /// QA server (which has no way to synthesize a native click on a collection-view item). No-op if
    /// the split view isn't built or `index` is out of range.
    #[cfg(target_os = "macos")]
    pub fn qa_select_grid_index(&mut self, index: usize, window: &winit::window::Window) {
        if let Some(split) = &self.split_view {
            // Update the model + native selection first, then record state. Mirrors the native
            // click order (`set_selected` in the delegate, then `BrowseGridSelected`).
            split.grid().select_index(index, /* scroll */ true);
        }
        self.set_grid_selected(index, window);
    }

    /// Pure state transition for a grid click/selection: focus the grid and record the index.
    /// Tested directly; the windowed `set_grid_selected` wraps it with a `sync_native`.
    fn focus_grid_state(&mut self, index: usize) {
        self.grid_selected = Some(index);
        self.focused_pane = Some(PaneSide::Grid);
    }

    /// Record a click/selection in the tree pane: move focus to the tree, then render. The folder
    /// selection itself rides `BrowseSelectFolder`; this keeps `focused_pane` the single source of
    /// truth so a tree click focuses the tree (and Tab then flips to the grid). No-op off macOS.
    #[cfg(target_os = "macos")]
    pub fn set_tree_focused(&mut self, window: &winit::window::Window) {
        self.focused_pane = Some(PaneSide::Tree);
        self.sync_native(window);
    }

    /// The grid's selected image path and the full folder image list, for opening into image mode.
    /// `None` when nothing is selected (empty grid). No-op off macOS.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn grid_open_target(&self) -> Option<(std::path::PathBuf, Vec<std::path::PathBuf>, usize)> {
        let split = self.split_view.as_ref()?;
        let grid = split.grid();
        let selected = grid.selected_index()?;
        let path = grid.selected_path()?;
        Some((path, grid.images(), selected))
    }

    /// The grid's full image list and the selected index, for warming the prospective current image
    /// and its neighbors into the image cache. Unlike `grid_open_target`, this is focus-independent
    /// (a selection lands the same whether it came from a grid click or a tree-arrow move). `None`
    /// when the split view isn't built or the grid has no selection (empty folder). No-op off macOS.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn grid_warm_target(&self) -> Option<(Vec<std::path::PathBuf>, usize)> {
        let grid = self.split_view.as_ref()?.grid();
        let selected = grid.selected_index()?;
        Some((grid.images(), selected))
    }

    // ── The Windows half ─────────────────────────────────────────────────────────────────
    //
    // Every method below is the Win32 twin of a macOS one a few lines up, and each is thin for
    // the same reason: the pure transitions they wrap (`enter_browse_state`, `toggle_focus_state`,
    // `focus_grid_state`) are shared, already tested, and platform-free. What differs is only the
    // native call at the end.

    /// Build the browser on first entry, then set state (`mode = Browse`, focus the grid when it
    /// has images else the tree) and render it.
    #[cfg(target_os = "windows")]
    pub fn enter_browse(&mut self, window: &winit::window::Window) {
        let Some(owner) = window_hwnd(window) else {
            return;
        };
        // `create` is idempotent and answers with the existing browser, so this is also what
        // rebuilds one whose window went away rather than leaving every later entry a no-op.
        self.browse_ui = windows::BrowseUi::create(owner, self.sort_by);
        crate::window::grow_to_browse_minimum(window);
        self.enter_browse_state(self.grid_is_empty());
        self.sync_native(window);
        log::info!("Entered browse mode, focused {:?}", self.focused_pane);
    }

    /// Leave browse mode for image mode.
    #[cfg(target_os = "windows")]
    pub fn enter_image(&mut self, window: &winit::window::Window) {
        self.enter_image_state();
        self.sync_native(window);
        log::info!("Entered image mode");
    }

    /// Switch to image mode for a browse→image reveal. Windows has no equivalent of the macOS
    /// hidden-layer trap (a swapchain isn't presented while the browser is up, so its last frame
    /// is whatever was there before), so this is the plain transition and the caller's paint
    /// lands on an uncovered surface.
    #[cfg(target_os = "windows")]
    pub fn reveal_image_canvas(&mut self, window: &winit::window::Window) {
        self.enter_image_state();
        self.sync_native(window);
    }

    /// Render every derived piece of the native browser from `State` — the one choke-point, the
    /// same as macOS's. Idempotent.
    #[cfg(target_os = "windows")]
    pub fn sync_native(&self, window: &winit::window::Window) {
        let _ = window;
        let Some(ui) = &self.browse_ui else {
            return;
        };
        ui.set_hidden(!self.mode.is_browse());
        if let Some(focused) = self.focused_pane {
            ui.apply_focus(focused);
        }
        windows::refresh_status_bar();
    }

    /// Re-fit the browser to the window's client area. Called on every resize, because the
    /// container is a child window and winit's own `WM_SIZE` doesn't reach it.
    #[cfg(target_os = "windows")]
    pub fn relayout(&self) {
        if let Some(ui) = &self.browse_ui {
            ui.relayout();
        }
    }

    /// Follow the monitor's scale factor to a new value.
    #[cfg(target_os = "windows")]
    pub fn rescale(&self) {
        if let Some(ui) = &self.browse_ui {
            ui.rescale();
        }
    }

    /// Reveal `folder` in the tree and preselect `current_image` in the grid once it lists.
    #[cfg(target_os = "windows")]
    pub fn reveal_to_folder(
        &mut self,
        folder: &std::path::Path,
        current_image: Option<std::path::PathBuf>,
    ) {
        self.pending_grid_preselect = current_image;
        self.pending_browse_open_focus = true;
        self.pending_reveal_folder = Some(folder.to_path_buf());
        if let Some(ui) = &self.browse_ui {
            ui.reveal_folder_in_tree(folder);
        }
    }

    /// Store a finished tree scan and put its children on the row.
    #[cfg(target_os = "windows")]
    pub fn tree_children_loaded(&self, path: &std::path::Path, subdirs: Vec<std::path::PathBuf>) {
        if let Some(ui) = &self.browse_ui {
            ui.tree_children_loaded(path, tree_model::visible_subdirs(subdirs));
        }
    }

    /// Apply a completed background folder listing to the grid.
    #[cfg(target_os = "windows")]
    pub fn grid_folder_listed(
        &mut self,
        folder: &std::path::Path,
        images: Vec<std::path::PathBuf>,
        window: &winit::window::Window,
    ) {
        let (preselect, was_browse_open) = self.take_browse_open_state(folder);
        if let Some(ui) = &self.browse_ui {
            ui.folder_listed(images, preselect.as_deref());
            self.grid_selected = ui.selected_index();
        }
        // Browse-open positioning focuses the GRID once its images land; a plain folder click
        // keeps the tree focused. Same rule as macOS.
        if was_browse_open && !self.grid_is_empty() {
            self.focused_pane = Some(PaneSide::Grid);
        }
        self.sync_native(window);
    }

    /// Drain queued thumbnail completions into the grid's image list.
    #[cfg(target_os = "windows")]
    pub fn grid_thumbnails_available(&self) {
        if self.browse_ui.is_some() {
            windows::thumbnails_available();
        }
    }

    /// Apply a live folder re-scan to the grid: keep the selection on the same file, refresh
    /// what changed, and re-sync. Returns `true` when the selected image changed identity, so
    /// the executor can re-warm it.
    #[cfg(target_os = "windows")]
    pub fn apply_grid_rescan(
        &mut self,
        images: Vec<std::path::PathBuf>,
        modified: &[std::path::PathBuf],
        window: &winit::window::Window,
    ) -> bool {
        let changed = self
            .browse_ui
            .as_ref()
            .is_some_and(|ui| ui.apply_rescan(images, modified));
        self.grid_selected = self
            .browse_ui
            .as_ref()
            .and_then(windows::BrowseUi::selected_index);
        self.sync_native(window);
        changed
    }

    /// Record a grid selection and move keyboard focus to the grid.
    #[cfg(target_os = "windows")]
    pub fn set_grid_selected(&mut self, index: usize, window: &winit::window::Window) {
        self.focus_grid_state(index);
        self.sync_native(window);
    }

    /// Select grid item `index` the way a click would, for the QA server.
    #[cfg(target_os = "windows")]
    pub fn qa_select_grid_index(&mut self, index: usize, window: &winit::window::Window) {
        if let Some(ui) = &self.browse_ui {
            ui.select_index(index, /* scroll */ true);
        }
        self.set_grid_selected(index, window);
    }

    /// Record a click in the tree pane.
    #[cfg(target_os = "windows")]
    pub fn set_tree_focused(&mut self, window: &winit::window::Window) {
        self.focused_pane = Some(PaneSide::Tree);
        self.sync_native(window);
    }

    /// The grid's selected image, the folder's full list, and the index, for opening into image
    /// mode.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub fn grid_open_target(&self) -> Option<(std::path::PathBuf, Vec<std::path::PathBuf>, usize)> {
        let ui = self.browse_ui.as_ref()?;
        let selected = ui.selected_index()?;
        let path = ui.selected_path()?;
        Some((path, ui.images(), selected))
    }

    /// The folder's images and the selected index, for warming the prospective current image.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub fn grid_warm_target(&self) -> Option<(Vec<std::path::PathBuf>, usize)> {
        let ui = self.browse_ui.as_ref()?;
        Some((ui.images(), ui.selected_index()?))
    }

    /// The tree's root paths, for the live-folder-sync watch.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub fn tree_root_paths(&self) -> Vec<std::path::PathBuf> {
        self.browse_ui
            .as_ref()
            .map(windows::BrowseUi::tree_root_paths)
            .unwrap_or_default()
    }

    /// Re-scan a watched tree folder after it changed on disk.
    #[cfg(target_os = "windows")]
    pub fn reload_tree_node(&self, folder: &std::path::Path) {
        if let Some(ui) = &self.browse_ui {
            ui.reload_tree_node(folder);
        }
    }

    /// True while the tree's reveal walk is still in flight, so QA can wait for it to settle.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub fn reveal_pending(&self) -> bool {
        self.browse_ui
            .as_ref()
            .is_some_and(windows::BrowseUi::reveal_pending)
    }

    /// How many images the grid lists.
    #[cfg(target_os = "windows")]
    #[must_use]
    pub fn grid_count(&self) -> usize {
        self.browse_ui
            .as_ref()
            .map_or(0, windows::BrowseUi::image_count)
    }

    /// The current top-level screen.
    #[must_use]
    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    /// True when browse mode is active.
    #[must_use]
    pub fn is_browse(&self) -> bool {
        self.mode.is_browse()
    }

    /// Which pane currently has keyboard focus. `None` in image mode.
    #[must_use]
    pub fn focused_pane(&self) -> Option<PaneSide> {
        self.focused_pane
    }

    /// Flip the focused pane (Tree ⇄ Grid) in state then render. Returns the new focused pane. The
    /// grid is skipped when empty (`next_focused_pane`), so Tab keeps focus on the tree — the grid
    /// is non-focusable until it has content. No-op when not in browse mode (`focused_pane` is
    /// `None`).
    pub fn toggle_focus(&mut self, window: &winit::window::Window) -> Option<PaneSide> {
        self.toggle_focus_state(self.grid_is_empty());
        log::debug!("Browse focus → {:?}", self.focused_pane);
        self.sync_native(window);
        self.focused_pane
    }

    /// Pure Tab transition: flip the focused pane, skipping an empty grid (`next_focused_pane`).
    /// No-op when not in browse mode (`focused_pane` is `None`). Tested directly; `toggle_focus`
    /// wraps it with a `sync_native`.
    fn toggle_focus_state(&mut self, grid_empty: bool) {
        if let Some(current) = self.focused_pane {
            self.focused_pane = Some(next_focused_pane(current, grid_empty));
        }
    }

    /// Render all derived native browse UI from `State` (the single source of truth) — the one
    /// choke-point through which browse UI changes. Idempotent: safe to call any number of times.
    /// Every browse mutation (`enter_browse`/`enter_image`, `toggle_focus`, grid/tree clicks, a
    /// folder listing) funnels through here rather than poking native views ad-hoc, so the native
    /// state can never drift from `State`. No-op off macOS.
    ///
    /// What it reads from state and what it sets:
    /// - **`mode`** → split-view visibility (shown iff Browse), the wgpu Metal layer hidden iff
    ///   Browse, and the image title/zoom labels hidden iff Browse (bug fix: in browse mode no
    ///   redraw fires, so without this the labels keep their last image-mode text and linger).
    /// - **`focused_pane`** → the window's first responder: the focused pane's native control in
    ///   browse (`NSOutlineView` for Tree, `NSCollectionView` for Grid), or the winit content view
    ///   in image mode (so winit owns the keyboard again — without it the hidden outline view keeps
    ///   the responder and image-mode keys are swallowed).
    /// - **Grid-selection invariant:** if the grid is the focused pane and has images but no live
    ///   selection, select a sensible index (the tracked `grid_selected`, else 0). This guarantees
    ///   the collection view always has a selection anchor when focused, so arrow keys work
    ///   immediately — fixing "Tab to the grid leaves arrows dead until you click a thumbnail".
    /// - **Emphasis:** the tree source list draws accent-blue while it's first responder
    ///   (automatic once the responder follows state); the grid's selected item draws blue iff the
    ///   grid is focused, gray otherwise (`refresh_focus_emphasis` repaints visible selected items).
    #[cfg(target_os = "macos")]
    pub fn sync_native(&self, window: &winit::window::Window) {
        let browse = self.mode.is_browse();

        // Split view + Metal layer visibility, derived from mode.
        if let Some(split) = &self.split_view {
            split.set_hidden(window, !browse);
        }
        crate::window::set_metal_layer_hidden(window, browse);

        // Image title/zoom labels: hidden in browse mode (browse stops redrawing, so the per-redraw
        // `set_titlebar_text` never runs to clear them and they'd linger over the native UI). In
        // image mode `set_view_mode` re-asserts their visibility against the title-bar/fullscreen
        // state, and the next redraw refreshes their text — so here we only ever hide, never force
        // them on (which would defy a title-bar-off / fullscreen setting).
        if browse {
            crate::window::set_titlebar_labels_hidden(window, true);
        }

        match self.focused_pane {
            Some(focused) => {
                if let Some(split) = &self.split_view {
                    // Grid-selection invariant: a focused, non-empty grid must have a selection
                    // anchor or its arrow keys do nothing until a click. Seed it from the tracked
                    // index (else 0) before making it first responder.
                    if focused == PaneSide::Grid {
                        let grid = split.grid();
                        if !grid.is_empty() && grid.selected_index().is_none() {
                            grid.select_index(self.grid_selected.unwrap_or(0), false);
                        }
                    }
                    split.apply_focus(focused);
                }
            }
            None => {
                // Image mode owns the keyboard: hand first responder back to winit so the hidden
                // outline view doesn't swallow keys (Enter→browse otherwise works only once).
                crate::window::restore_content_view_first_responder(window);
            }
        }
    }

    /// No-op where there's no native browser to render.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn sync_native(&self, _window: &winit::window::Window) {}

    /// Whether the grid currently has no images (so it can't receive focus). Treated as empty when
    /// the split view isn't built yet. Off macOS the grid doesn't exist, so always empty.
    #[must_use]
    fn grid_is_empty(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.split_view.as_ref().is_none_or(|s| s.grid().is_empty())
        }
        #[cfg(target_os = "windows")]
        {
            self.browse_ui
                .as_ref()
                .is_none_or(windows::BrowseUi::grid_is_empty)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            true
        }
    }

    /// Flip the mode and return the new value. Pure — callers (`enter_browse`/`enter_image`) update
    /// `focused_pane` and `sync_native` from the result.
    pub fn toggle_mode(&mut self) -> ViewMode {
        self.mode = self.mode.toggled();
        self.mode
    }

    /// Enter browse mode on the given winit window: build the split view on first use, set state
    /// (`mode = Browse`, focus the grid if it has images else the tree), then `sync_native` to
    /// render — show the split view, hide the Metal layer + image labels, make the focused pane
    /// first responder (with a grid-selection anchor). Also grows the window to the browse minimum
    /// if it's smaller (a small image may have shrunk it, leaving browse cramped). No-op off macOS.
    #[cfg(target_os = "macos")]
    pub fn enter_browse(&mut self, window: &winit::window::Window) {
        let sort_by = self.sort_by;
        self.split_view
            .get_or_insert_with(|| split_view::BrowseSplitView::create(window, sort_by));
        crate::window::grow_to_browse_minimum(window);
        self.enter_browse_state(self.grid_is_empty());
        self.sync_native(window);
        log::info!("Entered browse mode, focused {:?}", self.focused_pane);
    }

    /// The source-list root paths (home + volumes), for the live-folder-sync tree watch (roots stay
    /// watched for the window's life). Empty if the split view isn't built.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn tree_root_paths(&self) -> Vec<std::path::PathBuf> {
        self.split_view
            .as_ref()
            .map(split_view::BrowseSplitView::tree_root_paths)
            .unwrap_or_default()
    }

    /// Re-scan a watched (expanded) tree folder's subdirectories after it changed on disk and
    /// reload its node (live folder sync, Part B), preserving expansion + selection. No-op if the
    /// split view isn't built.
    #[cfg(target_os = "macos")]
    pub fn reload_tree_node(&self, folder: &std::path::Path) {
        if let Some(split) = &self.split_view {
            split.reload_tree_node(folder);
        }
    }

    /// Apply a completed folder scan to the tree: filter the scanned subdirectories down to the
    /// rows a sidebar shows (`tree_model::visible_subdirs`), store them, and reload that node so
    /// the outline view shows them. The tree ignores scans it didn't ask for — the scanner is
    /// shared, so results for image mode's folder land here too. No-op if the split view isn't
    /// built (browse never entered). Also refreshes the loading overlay.
    #[cfg(target_os = "macos")]
    pub fn tree_children_loaded(&self, path: &std::path::Path, subdirs: Vec<std::path::PathBuf>) {
        if let Some(split) = &self.split_view {
            split.tree_children_loaded(path, tree_model::visible_subdirs(subdirs));
        }
    }

    /// The longest-running still-in-flight tree scan (folder + start time), or `None` if no scan
    /// is pending (or the split view isn't built). The event loop uses the start time to schedule
    /// the loading-overlay timer and the folder to read the scan's live entry count.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn earliest_in_flight_scan(&self) -> Option<(std::path::PathBuf, std::time::Instant)> {
        self.split_view
            .as_ref()
            .and_then(split_view::BrowseSplitView::earliest_in_flight_scan)
    }

    /// Put the running scan status on the grid's centered label ("Scanning… 3,412 images so far"),
    /// or `None` to hand the label back to the "(No images)" rule. No-op if the split view isn't
    /// built.
    #[cfg(target_os = "macos")]
    pub fn set_grid_scan_status(&self, text: Option<&str>) {
        if let Some(split) = &self.split_view {
            split.set_grid_scan_status(text);
        }
    }

    /// Show or hide the tree-pane loading overlay based on whether a scan is overdue, and give it
    /// `scanned_count` entries-so-far when the caller has a live count. Called every
    /// `about_to_wait` so the overlay appears ~1 s into a slow scan, keeps its count current, and
    /// hides when the scan completes. No-op if the split view isn't built.
    #[cfg(target_os = "macos")]
    pub fn refresh_loading_overlay(&self, scanned_count: Option<usize>) {
        if let Some(split) = &self.split_view {
            split.refresh_loading_overlay(scanned_count);
        }
    }

    /// Enter image mode: set state (`mode = Image`, `focused_pane = None`) then `sync_native` to
    /// render — hide the split view, unhide the Metal layer + image labels, and hand first
    /// responder back to winit. No-op off macOS.
    #[cfg(target_os = "macos")]
    pub fn enter_image(&mut self, window: &winit::window::Window) {
        self.enter_image_state();
        self.sync_native(window);
        log::info!("Entered image mode");
    }

    /// Switch to image mode for a browse→image reveal: set image-mode state, hide the split view,
    /// restore winit's first responder, and UNHIDE the Metal layer. The caller then synchronously
    /// paints the target image (or a placeholder / black) into the now-visible drawable.
    ///
    /// **Why unhide first, not last.** Presenting to a hidden `CAMetalLayer` doesn't commit, so the
    /// old "paint-while-hidden then unhide" left the layer showing its last-VISIBLE frame for
    /// ~100 ms before the correct image landed. We instead guarantee that last-visible frame is
    /// black (made so on browse entry — `App::set_view_mode` clears the image and paints one frame
    /// while still visible), so the worst the user can see between unhide and paint is a brief
    /// black, never the stale stretched previous image. No-op off macOS.
    #[cfg(target_os = "macos")]
    pub fn reveal_image_canvas(&mut self, window: &winit::window::Window) {
        self.enter_image_state();
        // Hide the split view + image labels, hand first responder back to winit, and unhide the
        // Metal layer so the caller's synchronous paint is immediately visible.
        if let Some(split) = &self.split_view {
            split.set_hidden(window, true);
        }
        crate::window::restore_content_view_first_responder(window);
        crate::window::set_metal_layer_hidden(window, false);
    }

    /// Pure browse-entry transition: `mode = Browse`, focus the grid when it has images else the
    /// tree (`browse_entry_pane`). Tested directly; `enter_browse` wraps it with `sync_native`.
    fn enter_browse_state(&mut self, grid_empty: bool) {
        self.mode = ViewMode::Browse;
        self.focused_pane = Some(browse_entry_pane(grid_empty));
    }

    /// Pure image-entry transition: `mode = Image`, `focused_pane = None` (image mode owns the
    /// keyboard). Tested directly; `enter_image` wraps it with `sync_native`.
    fn enter_image_state(&mut self) {
        self.mode = ViewMode::Image;
        self.focused_pane = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_alternates_between_image_and_browse() {
        assert_eq!(ViewMode::Image.toggled(), ViewMode::Browse);
        assert_eq!(ViewMode::Browse.toggled(), ViewMode::Image);
        // Two toggles return to the start.
        assert_eq!(ViewMode::Image.toggled().toggled(), ViewMode::Image);
    }

    #[test]
    fn state_starts_in_image_mode() {
        let state = State::new();
        assert_eq!(state.mode(), ViewMode::Image);
        assert!(!state.is_browse());
    }

    #[test]
    fn toggle_mode_flips_and_reports_the_new_mode() {
        let mut state = State::new();
        assert_eq!(state.toggle_mode(), ViewMode::Browse);
        assert!(state.is_browse());
        assert_eq!(state.toggle_mode(), ViewMode::Image);
        assert!(!state.is_browse());
    }

    #[test]
    fn pane_side_toggles_between_tree_and_grid() {
        assert_eq!(PaneSide::Tree.toggled(), PaneSide::Grid);
        assert_eq!(PaneSide::Grid.toggled(), PaneSide::Tree);
        assert_eq!(PaneSide::Tree.toggled().toggled(), PaneSide::Tree);
    }

    #[test]
    fn state_starts_with_no_focused_pane_in_image_mode() {
        let state = State::new();
        assert_eq!(state.focused_pane(), None);
    }

    #[test]
    fn browse_entry_focuses_grid_when_it_has_images_else_tree() {
        // Entering browse must land on the GRID when the selected folder has images, so the gallery
        // is immediately keyboard-navigable. An empty grid is non-focusable, so we fall back to the
        // tree.
        assert_eq!(browse_entry_pane(/* grid_empty */ false), PaneSide::Grid);
        assert_eq!(browse_entry_pane(/* grid_empty */ true), PaneSide::Tree);
    }

    #[test]
    fn tab_toggles_tree_and_grid_when_grid_has_images() {
        assert_eq!(
            next_focused_pane(PaneSide::Tree, /* grid_empty */ false),
            PaneSide::Grid
        );
        assert_eq!(
            next_focused_pane(PaneSide::Grid, /* grid_empty */ false),
            PaneSide::Tree
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keydown_routes_only_tab_enter_esc_and_lets_arrows_fall_through() {
        use crate::commands::AppCommand;
        assert!(matches!(
            browse_keydown_command(key_codes::TAB),
            Some(AppCommand::ToggleBrowseFocus)
        ));
        assert!(matches!(
            browse_keydown_command(key_codes::ESCAPE),
            Some(AppCommand::EnterImageMode)
        ));
        assert!(matches!(
            browse_keydown_command(key_codes::RETURN),
            Some(AppCommand::BrowseOpenSelected)
        ));
        assert!(matches!(
            browse_keydown_command(key_codes::KEYPAD_ENTER),
            Some(AppCommand::BrowseOpenSelected)
        ));
        // Arrows (kVK_LeftArrow=123, …) and any other key fall through to native handling.
        assert!(browse_keydown_command(123).is_none());
        assert!(browse_keydown_command(125).is_none());
        assert!(browse_keydown_command(0).is_none());
    }

    #[test]
    fn browse_warm_indices_centers_on_selection_with_two_each_side() {
        // The selection comes first (the prospective current image), then ±2 neighbors. Order
        // beyond the first doesn't matter to the warmer, so compare as a set, but pin the first.
        let warm = browse_warm_indices(5, 10, 2);
        assert_eq!(
            warm[0], 5,
            "selection warms first (it's the prospective current)"
        );
        let set: std::collections::HashSet<usize> = warm.iter().copied().collect();
        assert_eq!(set, std::collections::HashSet::from([3, 4, 5, 6, 7]));
    }

    #[test]
    fn browse_warm_indices_clamps_at_folder_edges_and_does_not_wrap() {
        // At the first image, only the selection + the two ahead survive (no wrap to the end).
        let start = browse_warm_indices(0, 10, 2);
        let start_set: std::collections::HashSet<usize> = start.iter().copied().collect();
        assert_eq!(start_set, std::collections::HashSet::from([0, 1, 2]));

        // At the last image, only the selection + the two behind survive.
        let end = browse_warm_indices(9, 10, 2);
        let end_set: std::collections::HashSet<usize> = end.iter().copied().collect();
        assert_eq!(end_set, std::collections::HashSet::from([9, 8, 7]));
    }

    #[test]
    fn resolve_reveal_index_maps_grid_selection_to_dir_list_index() {
        use crate::navigation::SortBy;
        use std::path::PathBuf;

        // Grid hands an already-sorted list (Name order, natural alphanumeric). The selected grid
        // index must map 1:1 to the `from_explicit` re-sorted index, so reveal lands on the same
        // image the user picked.
        let images: Vec<PathBuf> = ["a.jpg", "b.jpg", "c.jpg", "d.jpg"]
            .iter()
            .map(PathBuf::from)
            .collect();
        for (i, _) in images.iter().enumerate() {
            assert_eq!(resolve_reveal_index(&images, i, SortBy::Name), Some(i));
        }
    }

    #[test]
    fn resolve_reveal_index_resolves_by_path_when_input_is_unsorted() {
        use crate::navigation::SortBy;
        use std::path::PathBuf;

        // Defensive: even if a caller passes an UNSORTED list, the index is resolved by the
        // selected path's position in the SORTED order — so the reveal still lands on the picked
        // file, not whatever happens to sit at that slot post-sort.
        let images: Vec<PathBuf> = ["c.jpg", "a.jpg", "b.jpg"]
            .iter()
            .map(PathBuf::from)
            .collect();
        // selected = 0 → "c.jpg", which sorts to position 2 (a, b, c).
        assert_eq!(resolve_reveal_index(&images, 0, SortBy::Name), Some(2));
        // selected = 1 → "a.jpg", sorts to position 0.
        assert_eq!(resolve_reveal_index(&images, 1, SortBy::Name), Some(0));
    }

    #[test]
    fn resolve_reveal_index_is_none_for_empty_or_out_of_range() {
        use crate::navigation::SortBy;
        use std::path::PathBuf;

        // Graceful no-selection: an empty folder or an out-of-range index yields `None`, so the
        // reveal keeps the current image instead of crashing or jumping to a wrong file.
        assert_eq!(resolve_reveal_index(&[], 0, SortBy::Name), None);
        let images: Vec<PathBuf> = vec![PathBuf::from("only.jpg")];
        assert_eq!(resolve_reveal_index(&images, 0, SortBy::Name), Some(0));
        assert_eq!(resolve_reveal_index(&images, 5, SortBy::Name), None);
    }

    #[test]
    fn grid_preselect_index_finds_the_current_image_else_none() {
        use std::path::PathBuf;
        let images: Vec<PathBuf> = ["a.jpg", "b.jpg", "c.jpg"]
            .iter()
            .map(PathBuf::from)
            .collect();
        // The current image is in the listed folder → preselect its exact slot (round-trip).
        assert_eq!(
            grid_preselect_index(&images, Some(std::path::Path::new("b.jpg"))),
            Some(1)
        );
        // Not in the list (different folder) → None, caller falls back to index 0.
        assert_eq!(
            grid_preselect_index(&images, Some(std::path::Path::new("zzz.jpg"))),
            None
        );
        // No current image at all → None.
        assert_eq!(grid_preselect_index(&images, None), None);
        // Empty folder → None regardless.
        assert_eq!(
            grid_preselect_index(&[], Some(std::path::Path::new("a.jpg"))),
            None
        );
    }

    /// Entering browse from an image on Windows: the displayed image carries the canonical
    /// `\\?\C:\…` spelling from its launch, while the grid lists the folder the tree selected,
    /// spelled the way `read_dir` produced it. Byte-matching finds nothing, so the grid preselects
    /// image 0 and Esc right after open lands somewhere you never were.
    #[test]
    fn grid_preselect_index_matches_a_came_from_image_in_any_windows_spelling() {
        use std::path::{Path, PathBuf};
        let listed: Vec<PathBuf> = [
            r"C:\Users\dave\pics\a.jpg",
            r"C:\Users\dave\pics\b.jpg",
            r"C:\Users\dave\pics\c.jpg",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let windows = crate::paths::PathPolicy::windows();
        assert_eq!(
            grid_preselect_index_under(
                windows,
                &listed,
                Some(Path::new(r"\\?\C:\Users\dave\pics\b.jpg")),
            ),
            Some(1)
        );
        // A different file is still a different file, however it's spelled.
        assert_eq!(
            grid_preselect_index_under(
                windows,
                &listed,
                Some(Path::new(r"\\?\C:\Users\dave\pics\zzz.jpg")),
            ),
            None
        );
        // And macOS keeps case apart: two names differing only in case are two files there.
        assert_eq!(
            grid_preselect_index_under(
                crate::paths::PathPolicy::macos(),
                &[PathBuf::from("/pics/B.jpg")],
                Some(Path::new("/pics/b.jpg")),
            ),
            None
        );
    }

    /// A Windows treeview picks its own first row the moment it takes focus, so the drive root
    /// lists before the reveal walk has gone anywhere. That listing must not spend the browse-open
    /// state, or the grid neither preselects the came-from image nor takes the focus.
    #[test]
    fn only_the_revealed_folders_listing_spends_the_browse_open_state() {
        use std::path::Path;
        let windows = crate::paths::PathPolicy::windows();
        let target = Path::new(r"\\?\C:\Users\dave\pics");

        // The drive root the control selected on its own: not ours, leave the state alone.
        assert!(!reveal_landing_matches(
            windows,
            Some(target),
            Path::new(r"C:\")
        ));
        // The reveal's own landing, spelled the way the tree row spells it.
        assert!(reveal_landing_matches(
            windows,
            Some(target),
            Path::new(r"C:\Users\dave\pics")
        ));
        // No reveal in flight — a plain folder click, which behaves as it always has.
        assert!(reveal_landing_matches(windows, None, Path::new(r"C:\any")));
    }

    #[test]
    fn browse_anchor_target_is_the_current_images_folder_and_the_image() {
        use std::path::{Path, PathBuf};
        // A current image → reveal its parent folder and preselect the image itself. This is the
        // "every browse entry re-anchors to the live current image" target.
        assert_eq!(
            browse_anchor_target(Some(Path::new("/photos/trip/b.jpg"))),
            Some((
                PathBuf::from("/photos/trip"),
                PathBuf::from("/photos/trip/b.jpg")
            ))
        );
        // No current image (nothing open) → no anchor; caller falls back to the last folder / home.
        assert_eq!(browse_anchor_target(None), None);
        // A path with no parent (a bare root) → no anchor (degenerate, can't happen for a real
        // image path, but stays graceful rather than panicking).
        assert_eq!(browse_anchor_target(Some(Path::new("/"))), None);
    }

    #[test]
    fn classify_launch_target_picks_image_dir_or_onboarding() {
        // A file → image mode (today's behavior, unchanged).
        assert_eq!(
            classify_launch_target(/* is_file */ true, /* is_dir */ false),
            LaunchTarget::Image
        );
        // A directory → browse mode.
        assert_eq!(
            classify_launch_target(/* is_file */ false, /* is_dir */ true),
            LaunchTarget::Directory
        );
        // Neither (missing / unreadable) → onboarding, like a no-arg launch.
        assert_eq!(
            classify_launch_target(/* is_file */ false, /* is_dir */ false),
            LaunchTarget::Onboarding
        );
        // A path reporting both (can't happen in practice) → file wins.
        assert_eq!(
            classify_launch_target(/* is_file */ true, /* is_dir */ true),
            LaunchTarget::Image
        );
    }

    #[test]
    fn browse_warm_indices_always_includes_the_focused_index_first() {
        // Pre-warm guarantee: the focused (selected) image itself must be in the warm set, and
        // first, so revealing it is a cache hit (black → image, near-instant) rather than a
        // cache-miss placeholder. Holds at the edges and the interior alike.
        for (selected, total, radius) in [(0, 1, 1), (0, 10, 2), (5, 10, 1), (9, 10, 2), (3, 4, 1)]
        {
            let warm = browse_warm_indices(selected, total, radius);
            assert_eq!(
                warm.first().copied(),
                Some(selected),
                "focused index {selected} (total {total}) must warm first"
            );
            assert!(
                warm.contains(&selected),
                "focused index {selected} (total {total}) must be in the warm set"
            );
        }
    }

    /// On a machine whose cache budget only retains three images, warming has
    /// to narrow with it — otherwise browse re-fills the same cache the image
    /// mode preloader is trying to keep warm.
    #[test]
    fn browse_warm_indices_narrows_with_the_radius() {
        let warm = browse_warm_indices(5, 10, 1);
        assert_eq!(warm[0], 5, "selection still warms first");
        let set: std::collections::HashSet<usize> = warm.iter().copied().collect();
        assert_eq!(set, std::collections::HashSet::from([4, 5, 6]));
    }

    #[test]
    fn browse_warm_indices_handles_empty_and_out_of_range() {
        assert!(browse_warm_indices(0, 0, 2).is_empty());
        assert!(browse_warm_indices(5, 3, 2).is_empty());
        // A single-image folder warms just that image.
        assert_eq!(browse_warm_indices(0, 1, 2), vec![0]);
    }

    #[test]
    fn tab_skips_an_empty_grid_and_stays_on_the_tree() {
        // From the tree, Tab toward an empty grid stays on the tree (the grid can't take focus).
        assert_eq!(
            next_focused_pane(PaneSide::Tree, /* grid_empty */ true),
            PaneSide::Tree
        );
        // From the grid (it had images, then emptied), Tab still moves back to the tree.
        assert_eq!(
            next_focused_pane(PaneSide::Grid, /* grid_empty */ true),
            PaneSide::Tree
        );
    }

    // ── Render-from-state: the pure state transitions behind the windowed mutators ──
    // These assert `State`'s fields after each browse transition (the single source of truth).
    // The derived native side (`sync_native`) is objc2, covered by the smoke run + live QA.

    #[test]
    fn enter_browse_state_focuses_grid_with_images_else_tree() {
        let mut state = State::new();
        state.enter_browse_state(/* grid_empty */ false);
        assert_eq!(state.mode(), ViewMode::Browse);
        assert_eq!(state.focused_pane(), Some(PaneSide::Grid));

        let mut empty = State::new();
        empty.enter_browse_state(/* grid_empty */ true);
        assert_eq!(empty.mode(), ViewMode::Browse);
        assert_eq!(empty.focused_pane(), Some(PaneSide::Tree));
    }

    #[test]
    fn enter_image_state_clears_focus() {
        let mut state = State::new();
        state.enter_browse_state(false);
        state.enter_image_state();
        assert_eq!(state.mode(), ViewMode::Image);
        // Image mode owns the keyboard — no pane is focused.
        assert_eq!(state.focused_pane(), None);
    }

    #[test]
    fn focus_grid_state_focuses_grid_and_records_the_index() {
        let mut state = State::new();
        state.enter_browse_state(false); // start focused on the grid
        state.toggle_focus_state(false); // …Tab to the tree
        assert_eq!(state.focused_pane(), Some(PaneSide::Tree));
        // A grid click moves focus back to the grid and records the clicked index.
        state.focus_grid_state(7);
        assert_eq!(state.focused_pane(), Some(PaneSide::Grid));
        assert_eq!(state.grid_selected(), Some(7));
    }

    #[test]
    fn toggle_focus_state_flips_panes_and_skips_an_empty_grid() {
        let mut state = State::new();
        state.enter_browse_state(false); // grid focused, grid non-empty
        state.toggle_focus_state(false);
        assert_eq!(state.focused_pane(), Some(PaneSide::Tree));
        state.toggle_focus_state(false);
        assert_eq!(state.focused_pane(), Some(PaneSide::Grid));

        // On the tree with an empty grid, Tab stays put (the grid is non-focusable).
        let mut empty = State::new();
        empty.enter_browse_state(true); // tree focused, grid empty
        empty.toggle_focus_state(true);
        assert_eq!(empty.focused_pane(), Some(PaneSide::Tree));
    }

    #[test]
    fn toggle_focus_state_is_a_noop_in_image_mode() {
        // No focused pane in image mode, so Tab has nothing to flip.
        let mut state = State::new();
        state.toggle_focus_state(false);
        assert_eq!(state.focused_pane(), None);
    }
}
