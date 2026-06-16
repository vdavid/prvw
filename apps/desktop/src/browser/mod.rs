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
//! - Focus: on entering browse, `makeFirstResponder:` the left pane. Tab toggles focus between
//!   the panes via the native key-view loop. Esc / Enter in browse return to image mode through
//!   a `define_class!` container that overrides `keyDown:` and sends `AppCommand`s.

#[cfg(target_os = "macos")]
mod split_view;

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

/// Per-feature browse-mode state (sibling of `zoom::State`, `navigation::State`, …). Holds the
/// current `ViewMode` and, on macOS, the native split-view handles built lazily on first entry.
pub struct State {
    /// Current top-level screen. Starts in `Image`.
    mode: ViewMode,
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
            #[cfg(target_os = "macos")]
            split_view: None,
        }
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
        let split = self
            .split_view
            .get_or_insert_with(|| split_view::BrowseSplitView::create(window));
        split.set_hidden(window, false);
        crate::window::set_metal_layer_hidden(window, true);
        split.focus_tree(window);
        log::info!("Entered browse mode");
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
}
