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
//! - Keyboard: winit keeps delivering `WindowEvent::KeyboardInput` even while the native split
//!   view is up, so browse-mode keys flow through winit → `input::browse_key_to_command` →
//!   `AppCommand`, branched by mode (Tab → `ToggleBrowseFocus`, Esc/Enter → `EnterImageMode`).
//!   Focus is the app-tracked `focused_pane`, not the native key-view loop; the split view just
//!   highlights whichever pane the value names. See `docs/specs/image-browser.md`.

#[cfg(target_os = "macos")]
mod outline;
#[cfg(target_os = "macos")]
mod split_view;
mod tree_model;

#[cfg(target_os = "macos")]
pub use tree_model::count_supported_images;

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

/// Which of the two browse panes has keyboard focus. Tab flips it. Tracked here
/// (app-managed), not via the AppKit key-view loop — winit keeps the keyboard even
/// while the native split view is up, so focus has to be a value we own and drive the
/// native views from. See `docs/specs/image-browser.md` → "Input architecture".
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

/// Per-feature browse-mode state (sibling of `zoom::State`, `navigation::State`, …). Holds the
/// current `ViewMode` and, on macOS, the native split-view handles built lazily on first entry.
pub struct State {
    /// Current top-level screen. Starts in `Image`.
    mode: ViewMode,
    /// Which pane has keyboard focus in browse mode. Tab flips it (app-managed, not the
    /// native key-view loop). Entering browse starts on the tree.
    focused_pane: PaneSide,
    /// The folder currently selected in the tree. Set by `BrowseSelectFolder`; the grid will
    /// list its images in a later phase. `None` until the user picks a folder.
    selected_folder: Option<std::path::PathBuf>,
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
            focused_pane: PaneSide::Tree,
            selected_folder: None,
            #[cfg(target_os = "macos")]
            split_view: None,
        }
    }

    /// The folder currently selected in the browse-mode tree, if any.
    #[must_use]
    pub fn selected_folder(&self) -> Option<&std::path::Path> {
        self.selected_folder.as_deref()
    }

    /// Record the folder selected in the tree. The grid listing lands in a later phase; for now
    /// the app logs how many supported images it holds.
    pub fn set_selected_folder(&mut self, folder: std::path::PathBuf) {
        self.selected_folder = Some(folder);
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

    /// Which pane currently has keyboard focus in browse mode.
    #[must_use]
    pub fn focused_pane(&self) -> PaneSide {
        self.focused_pane
    }

    /// Flip the focused pane (Tree ⇄ Grid) and apply the new highlight to the native
    /// views. Returns the new focused pane. No-op off macOS for the native side.
    pub fn toggle_focus(&mut self, #[allow(unused)] window: &winit::window::Window) -> PaneSide {
        self.focused_pane = self.focused_pane.toggled();
        #[cfg(target_os = "macos")]
        if let Some(split) = &self.split_view {
            split.set_focused_pane(self.focused_pane);
        }
        log::debug!("Browse focus → {:?}", self.focused_pane);
        self.focused_pane
    }

    /// Flip the mode and return the new value. Pure — callers do the AppKit side effects
    /// (show/hide the split view and Metal layer) based on the result.
    pub fn toggle_mode(&mut self) -> ViewMode {
        self.mode = self.mode.toggled();
        self.mode
    }

    /// Enter browse mode on the given winit window: build the split view on first use, unhide it,
    /// hide the wgpu Metal layer, and focus the tree pane. No-op off macOS.
    #[cfg(target_os = "macos")]
    pub fn enter_browse(&mut self, window: &winit::window::Window) {
        self.focused_pane = PaneSide::Tree;
        let split = self
            .split_view
            .get_or_insert_with(|| split_view::BrowseSplitView::create(window));
        split.set_hidden(window, false);
        crate::window::set_metal_layer_hidden(window, true);
        split.set_focused_pane(self.focused_pane);
        log::info!("Entered browse mode");
    }

    /// Move the tree selection (arrow Up/Down) when the tree pane is focused. `delta` is +1
    /// (Down) or -1 (Up). No-op if the grid pane is focused or the split view isn't built.
    #[cfg(target_os = "macos")]
    pub fn move_tree_selection(&self, delta: i32) {
        if self.focused_pane != PaneSide::Tree {
            return;
        }
        if let Some(split) = &self.split_view {
            split.move_tree_selection(delta);
        }
    }

    /// Expand (`true`, Right arrow) or collapse (`false`, Left arrow) the selected tree row,
    /// when the tree pane is focused. No-op otherwise.
    #[cfg(target_os = "macos")]
    pub fn expand_tree_selection(&self, expand: bool) {
        if self.focused_pane != PaneSide::Tree {
            return;
        }
        if let Some(split) = &self.split_view {
            if expand {
                split.expand_tree_selection();
            } else {
                split.collapse_tree_selection();
            }
        }
    }

    /// Enter image mode: hide the split view (if built) and unhide the Metal layer. No-op off
    /// macOS.
    #[cfg(target_os = "macos")]
    pub fn enter_image(&mut self, window: &winit::window::Window) {
        if let Some(split) = &self.split_view {
            split.set_hidden(window, true);
        }
        crate::window::set_metal_layer_hidden(window, false);
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
    fn state_starts_focused_on_the_tree_pane() {
        let state = State::new();
        assert_eq!(state.focused_pane(), PaneSide::Tree);
    }
}
