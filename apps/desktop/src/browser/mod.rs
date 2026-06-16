//! # Browser (browse mode)
//!
//! A second top-level screen for the main window: a native AppKit `NSSplitView` (folder tree
//! on the left, thumbnail grid on the right) that **swaps** with the wgpu image viewer. Image
//! mode is the GPU surface; browse mode is fully native AppKit. They never overlap — a
//! transparent Metal pixel still occludes in-window content behind it, so we hide one and show
//! the other (see the "Native AppKit views over/around the wgpu Metal layer" gotcha in
//! `platform/macos/CLAUDE.md`).
//!
//! This is the Phase 0 **spike**: the split view, both panes, and the focus/keyboard plumbing
//! are stubs proving the AppKit-in-the-winit-loop mechanics. The real tree/grid content lands
//! in later phases. See `docs/specs/image-browser.md`.
//!
//! ## What the spike establishes
//!
//! - The split view is a **sibling subview of winit's contentView** (same pattern as
//!   `window::add_titlebar_labels`): `addSubview`, layer-backed, `zPosition` above the Metal
//!   layer's `1.0`, pinned to the contentView edges, with a stable `identifier` for hide/show.
//! - Entering browse: unhide the split view, hide the Metal layer
//!   (`window::set_metal_layer_hidden`), stop requesting redraws. Entering image: reverse.
//! - Keyboard: in idle-winit browse mode the focused native view holds first responder and
//!   handles its own arrows natively. `focused_pane: Option<PaneSide>` is the single source of
//!   truth; `apply_focus` keeps the native first responder synced to it. The focused view's
//!   `keyDown:` override handles only Tab/Enter/Esc (routed via `AppCommand`); everything else
//!   falls through to `super` for native selection/scroll. See `docs/specs/image-browser.md`.

#[cfg(target_os = "macos")]
mod grid;
#[cfg(target_os = "macos")]
mod grid_listing;
pub(crate) mod grid_model;
#[cfg(target_os = "macos")]
mod outline;
#[cfg(target_os = "macos")]
mod split_view;
pub(crate) mod tree_model;

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

/// Per-feature browse-mode state (sibling of `zoom::State`, `navigation::State`, …). Holds the
/// current `ViewMode` and, on macOS, the native split-view handles built lazily on first entry.
pub struct State {
    /// Current top-level screen. Starts in `Image`.
    mode: ViewMode,
    /// The single source of truth for which pane is focused: `None` in image mode, `Some(Tree)` /
    /// `Some(Grid)` in browse mode. `apply_focus` syncs the native first responder to it. Never
    /// inferred from the native first responder.
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
    /// The native split view + its panes, built once on first entry to browse mode and kept
    /// alive for the window's lifetime thereafter. `None` until first built.
    #[cfg(target_os = "macos")]
    split_view: Option<split_view::BrowseSplitView>,
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
            #[cfg(target_os = "macos")]
            split_view: None,
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

    /// The folder currently selected in the browse-mode tree, if any.
    #[must_use]
    pub fn selected_folder(&self) -> Option<&std::path::Path> {
        self.selected_folder.as_deref()
    }

    /// Record the folder selected in the tree and begin listing its images for the grid on the
    /// background worker (the result arrives as `AppCommand::BrowseFolderListed`). Never reads the
    /// disk here — a slow folder selection must not freeze the UI.
    pub fn set_selected_folder(&mut self, folder: std::path::PathBuf) {
        self.selected_folder = Some(folder.clone());
        #[cfg(target_os = "macos")]
        if let Some(split) = &self.split_view {
            split.grid().list_folder(folder);
        }
    }

    /// Apply a completed background folder listing to the grid: populate the model, reload the
    /// collection view, toggle the empty overlay, and start thumbnail generation. Updates the
    /// tracked grid selection. No-op if the split view isn't built.
    #[cfg(target_os = "macos")]
    pub fn grid_folder_listed(&mut self, images: Vec<std::path::PathBuf>) {
        if let Some(split) = &self.split_view {
            split.grid().folder_listed(images);
            self.grid_selected = split.grid().selected_index();
        }
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
    /// scroll. No-op if the split view isn't built or the grid is empty.
    #[cfg(target_os = "macos")]
    pub fn grid_pump_visible_range(&self) {
        if let Some(split) = &self.split_view {
            split.grid().pump_visible_range();
        }
    }

    /// Record a grid selection (native click or programmatic) and move keyboard focus to the grid
    /// pane — a click in the grid focuses it, so a following Enter / double-click opens that image.
    /// Mirrors the index into `State` for QA/tests.
    #[cfg(target_os = "macos")]
    pub fn set_grid_selected(&mut self, index: usize) {
        self.grid_selected = Some(index);
        if self.focused_pane != Some(PaneSide::Grid) {
            self.focused_pane = Some(PaneSide::Grid);
            self.apply_focus();
        }
    }

    /// Record a click in the tree pane: move focus to the tree. The selection itself rides the
    /// `BrowseSelectFolder` command; this just keeps `focused_pane` the single source of truth so a
    /// click in the tree focuses it (and Tab then flips to the grid). No-op off macOS.
    #[cfg(target_os = "macos")]
    pub fn set_tree_focused(&mut self) {
        if self.focused_pane != Some(PaneSide::Tree) {
            self.focused_pane = Some(PaneSide::Tree);
            self.apply_focus();
        }
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

    /// Flip the focused pane (Tree ⇄ Grid) and sync the native first responder + emphasis via
    /// `apply_focus`. Returns the new focused pane. The grid is skipped when empty (no images to
    /// focus), so Tab keeps focus on the tree — the grid is non-focusable until it has content.
    /// No-op when not in browse mode (`focused_pane` is `None`).
    pub fn toggle_focus(&mut self, #[allow(unused)] window: &winit::window::Window) -> Option<PaneSide> {
        let Some(current) = self.focused_pane else {
            return None;
        };
        let next = next_focused_pane(current, self.grid_is_empty());
        if next == current {
            log::debug!("Browse focus stays on tree (grid empty)");
            return self.focused_pane;
        }
        self.focused_pane = Some(next);
        self.apply_focus();
        log::debug!("Browse focus → {:?}", self.focused_pane);
        self.focused_pane
    }

    /// Sync the native first responder + emphasis to `focused_pane`: `makeFirstResponder:` the
    /// focused pane's native view (the `NSOutlineView` for Tree, the `NSCollectionView` for Grid)
    /// so it draws native selection emphasis and handles its own arrows, and refresh the focus cue
    /// on both panes. Call on every focus change. No-op off macOS, or when `focused_pane` is `None`
    /// (image mode owns the keyboard then).
    #[cfg(target_os = "macos")]
    pub fn apply_focus(&self) {
        let Some(focused) = self.focused_pane else {
            return;
        };
        if let Some(split) = &self.split_view {
            split.apply_focus(focused);
        }
    }

    /// No-op off macOS (no native views to focus).
    #[cfg(not(target_os = "macos"))]
    pub fn apply_focus(&self) {}

    /// Whether the grid currently has no images (so it can't receive focus). Treated as empty when
    /// the split view isn't built yet. Off macOS the grid doesn't exist, so always empty.
    #[must_use]
    fn grid_is_empty(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.split_view.as_ref().is_none_or(|s| s.grid().is_empty())
        }
        #[cfg(not(target_os = "macos"))]
        {
            true
        }
    }

    /// Flip the mode and return the new value. Pure — callers do the AppKit side effects
    /// (show/hide the split view and Metal layer) based on the result.
    pub fn toggle_mode(&mut self) -> ViewMode {
        self.mode = self.mode.toggled();
        self.mode
    }

    /// Enter browse mode on the given winit window: build the split view on first use, unhide it,
    /// hide the wgpu Metal layer, and focus the grid if the selected folder has images (else the
    /// tree). `apply_focus` makes the focused native view first responder so its arrows work
    /// natively. No-op off macOS.
    #[cfg(target_os = "macos")]
    pub fn enter_browse(&mut self, window: &winit::window::Window) {
        let sort_by = self.sort_by;
        let split = self
            .split_view
            .get_or_insert_with(|| split_view::BrowseSplitView::create(window, sort_by));
        split.set_hidden(window, false);
        crate::window::set_metal_layer_hidden(window, true);
        // Focus the grid when it has images, else the tree (an empty grid is non-focusable).
        self.focused_pane = Some(browse_entry_pane(self.grid_is_empty()));
        self.apply_focus();
        log::info!("Entered browse mode, focused {:?}", self.focused_pane);
    }

    /// Apply a completed background directory scan to the tree: store the children and reload that
    /// node so the outline view shows them. No-op if the split view isn't built (browse never
    /// entered). Also refreshes the loading overlay.
    #[cfg(target_os = "macos")]
    pub fn tree_children_loaded(&self, path: &std::path::Path, children: Vec<std::path::PathBuf>) {
        if let Some(split) = &self.split_view {
            split.tree_children_loaded(path, children);
        }
    }

    /// The earliest still-in-flight tree-scan start time, or `None` if no scan is pending (or the
    /// split view isn't built). The event loop uses this to schedule the loading-overlay timer.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn earliest_in_flight_scan(&self) -> Option<std::time::Instant> {
        self.split_view
            .as_ref()
            .and_then(split_view::BrowseSplitView::earliest_in_flight_scan)
    }

    /// Show or hide the tree-pane loading overlay based on whether a scan is overdue. Called every
    /// `about_to_wait` so the overlay appears ~1 s into a slow scan and hides when it completes.
    /// No-op if the split view isn't built.
    #[cfg(target_os = "macos")]
    pub fn refresh_loading_overlay(&self) {
        if let Some(split) = &self.split_view {
            split.refresh_loading_overlay();
        }
    }

    /// Enter image mode: hide the split view (if built) and unhide the Metal layer. No-op off
    /// macOS.
    #[cfg(target_os = "macos")]
    pub fn enter_image(&mut self, window: &winit::window::Window) {
        self.focused_pane = None;
        if let Some(split) = &self.split_view {
            split.set_hidden(window, true);
        }
        crate::window::set_metal_layer_hidden(window, false);
        // Hand the keyboard back to winit: the hidden outline view may still hold first responder,
        // which would swallow image-mode keys (Enter does nothing). Without this, Enter→browse only
        // works once — see `window::restore_content_view_first_responder`.
        crate::window::restore_content_view_first_responder(window);
        log::info!("Entered image mode");
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
}
