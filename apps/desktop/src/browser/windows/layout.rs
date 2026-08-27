//! Where the Windows browse panes go, in device pixels at the monitor's own DPI.
//!
//! Pure, and compiled on every platform, so a Mac asserts the geometry the Win32 layer will
//! apply. `ui` does nothing with these numbers but hand them to `SetWindowPos`.
//!
//! The shape is Explorer's and ACDSee's: a folder tree on the left, a hand-written splitter, a
//! thumbnail grid filling the rest, and a status bar across the bottom. The status bar is the one
//! piece with no macOS counterpart, and it's Windows-only on purpose
//! (`docs/specs/windows-ui-design.md` → "The browse-mode status bar").
//!
//! ## Decision: this module carries its own `Rect` and `scale`
//!
//! **Why:** `settings::windows::layout` has the twin, and one shared copy would be better. It
//! would have to live somewhere both a Windows build and a Mac's test run can reach, which is
//! neither `platform::windows` (Windows-only) nor either feature's own module. Inventing that
//! home means editing the settings dialog's layout module, which is live work in another
//! worktree, so the merge cost outweighs twenty lines of geometry.

/// A box in device pixels, top-left origin — the coordinate space `SetWindowPos` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn right(&self) -> i32 {
        self.x + self.width
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    /// True when the two boxes share any pixel. What the tests hold the layout to: no pane is
    /// ever drawn on top of another, at any window size or scale factor.
    #[cfg(test)]
    pub const fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// Logical pixels to device pixels, the way `MulDiv` does it: multiply, divide, round to
/// nearest. Every number below that started life as a constant goes through it.
#[must_use]
pub fn scale(value: i32, dpi: u32) -> i32 {
    // `GetDpiForWindow` answers 0 when it can't tell, and 96 is what "can't tell" means.
    let dpi = i64::from(if dpi == 0 { 96 } else { dpi });
    let scaled = i64::from(value) * dpi + 96 / 2;
    i32::try_from(scaled / 96).unwrap_or(i32::MAX)
}

/// The tree pane's width on first entry, in logical pixels. Matches the macOS browser's sidebar,
/// which is the one measurement worth carrying over: it's wide enough for a nested folder name
/// and narrow enough to leave the grid three columns on a laptop.
pub const TREE_PANE_DEFAULT: i32 = 240;

/// How narrow the tree pane may be dragged before it stops. Below this a folder name is all
/// ellipsis and the pane is useless rather than compact.
pub const TREE_PANE_MIN: i32 = 120;

/// How narrow the grid pane may be squeezed. One thumbnail column plus its scrollbar.
pub const GRID_PANE_MIN: i32 = 200;

/// The splitter's thickness in logical pixels. Explorer's is a hairline with a wide hit area;
/// ours is the hit area, drawn in the window background so it reads as a gap.
pub const SPLITTER_WIDTH: i32 = 6;

/// One grid cell's width in logical pixels, thumbnail plus the padding around it.
pub const CELL_WIDTH: i32 = 160;

/// The thumbnail inside a cell, in logical pixels. `HIMAGELIST` icons are one fixed size, so
/// this is what every thumbnail is generated and stored at.
pub const CELL_THUMBNAIL: i32 = 128;

/// Room under the thumbnail for one line of filename, in logical pixels.
pub const CELL_LABEL_HEIGHT: i32 = 34;

/// The browse container's measurements at one monitor's DPI, in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// The splitter's thickness.
    pub splitter: i32,
    /// The narrowest the tree pane may be.
    pub min_tree: i32,
    /// The narrowest the grid pane may be.
    pub min_grid: i32,
    /// The status bar's height, as the control itself reported it. Windows sizes a status bar
    /// from the system font, so it is read rather than computed.
    pub status_bar: i32,
}

impl Metrics {
    /// The measurements at `dpi`, given the height the status bar reported.
    #[must_use]
    pub fn for_dpi(dpi: u32, status_bar_height: i32) -> Self {
        Self {
            splitter: scale(SPLITTER_WIDTH, dpi),
            min_tree: scale(TREE_PANE_MIN, dpi),
            min_grid: scale(GRID_PANE_MIN, dpi),
            status_bar: status_bar_height.max(0),
        }
    }
}

/// Where each child window goes inside the browse container's client area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub tree: Rect,
    pub splitter: Rect,
    pub grid: Rect,
    pub status_bar: Rect,
}

/// The tree width to actually use, given what the user has dragged it to and how much room there
/// is. Both minimums are honoured while the window is wide enough to hold them; once it isn't,
/// the grid's minimum wins and the tree gives up whatever is left, down to nothing. A pane of
/// negative width isn't a thing `SetWindowPos` can express, so nothing here ever returns one.
#[must_use]
pub fn clamp_tree_width(desired: i32, client_width: i32, metrics: Metrics) -> i32 {
    let widest = (client_width - metrics.splitter - metrics.min_grid).max(0);
    if widest <= metrics.min_tree {
        return widest;
    }
    desired.clamp(metrics.min_tree, widest)
}

/// Lay the four children out in a `client_width` × `client_height` container. `tree_width` is
/// taken as given: the caller has already put it through [`clamp_tree_width`], which is also
/// what a splitter drag calls.
#[must_use]
pub fn layout(client_width: i32, client_height: i32, tree_width: i32, metrics: Metrics) -> Layout {
    let width = client_width.max(0);
    let height = client_height.max(0);
    // The status bar takes its height off the bottom, and never more than the whole window.
    let status_height = metrics.status_bar.min(height);
    let content_height = height - status_height;
    let tree_width = tree_width.clamp(0, width);
    let splitter_width = metrics.splitter.min(width - tree_width);

    Layout {
        tree: Rect {
            x: 0,
            y: 0,
            width: tree_width,
            height: content_height,
        },
        splitter: Rect {
            x: tree_width,
            y: 0,
            width: splitter_width,
            height: content_height,
        },
        grid: Rect {
            x: tree_width + splitter_width,
            y: 0,
            width: width - tree_width - splitter_width,
            height: content_height,
        },
        status_bar: Rect {
            x: 0,
            y: content_height,
            width,
            height: status_height,
        },
    }
}

/// The tree width a splitter drag lands on. `pointer_x` is the pointer's position in the
/// container's client coordinates and `grab_offset` is how far into the splitter the drag
/// started, so the splitter stays under the same pixel of the pointer for the whole drag rather
/// than jumping to centre itself on the first move.
#[must_use]
pub fn tree_width_for_drag(
    pointer_x: i32,
    grab_offset: i32,
    client_width: i32,
    metrics: Metrics,
) -> i32 {
    clamp_tree_width(pointer_x - grab_offset, client_width, metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1440 × 900 window at 100 %, with a 22-pixel status bar.
    fn metrics() -> Metrics {
        Metrics::for_dpi(96, 22)
    }

    #[test]
    fn logical_pixels_scale_the_way_muldiv_does() {
        assert_eq!(scale(240, 96), 240);
        assert_eq!(scale(240, 144), 360); // 150 %
        assert_eq!(scale(240, 192), 480); // 200 %
        // Rounds to nearest rather than truncating: 6 logical at 125 % is 7.5.
        assert_eq!(scale(6, 120), 8);
        // `GetDpiForWindow` answers 0 when it can't tell, which means 100 %.
        assert_eq!(scale(10, 0), 10);
    }

    #[test]
    fn the_four_panes_tile_the_client_area_with_no_gap_and_no_overlap() {
        let m = metrics();
        let l = layout(1440, 900, 240, m);

        assert_eq!(
            l.tree,
            Rect {
                x: 0,
                y: 0,
                width: 240,
                height: 878
            }
        );
        assert_eq!(l.splitter.x, 240);
        assert_eq!(l.splitter.width, 6);
        assert_eq!(l.grid.x, 246);
        assert_eq!(l.grid.right(), 1440);
        assert_eq!(
            l.status_bar,
            Rect {
                x: 0,
                y: 878,
                width: 1440,
                height: 22
            }
        );

        // Nothing overlaps, and the three content panes span the full width.
        for (a, b) in [
            (l.tree, l.splitter),
            (l.splitter, l.grid),
            (l.tree, l.grid),
            (l.grid, l.status_bar),
            (l.tree, l.status_bar),
        ] {
            assert!(!a.overlaps(&b), "{a:?} overlaps {b:?}");
        }
        assert_eq!(l.tree.width + l.splitter.width + l.grid.width, 1440);
        assert_eq!(l.grid.bottom(), l.status_bar.y);
        assert_eq!(l.status_bar.bottom(), 900);
    }

    /// Every DPI the panes have to tile at. A rounding error here shows up as a one-pixel stripe
    /// of window background between two controls.
    #[test]
    fn the_panes_tile_at_every_scale_factor() {
        for dpi in [96, 120, 144, 168, 192, 240] {
            let m = Metrics::for_dpi(dpi, scale(22, dpi));
            let width = scale(1200, dpi);
            let height = scale(800, dpi);
            let tree = clamp_tree_width(scale(TREE_PANE_DEFAULT, dpi), width, m);
            let l = layout(width, height, tree, m);
            assert_eq!(
                l.tree.width + l.splitter.width + l.grid.width,
                width,
                "at {dpi} DPI"
            );
            assert_eq!(
                l.grid.bottom() + l.status_bar.height,
                height,
                "at {dpi} DPI"
            );
            assert!(!l.tree.overlaps(&l.grid), "at {dpi} DPI");
        }
    }

    #[test]
    fn a_drag_keeps_the_splitter_under_the_pointer() {
        let m = metrics();
        // Grabbed 3 pixels into the splitter, dragged to x = 403: the tree ends at 400.
        assert_eq!(tree_width_for_drag(403, 3, 1440, m), 400);
        // Dragged left past the tree's minimum: it stops there rather than vanishing.
        assert_eq!(tree_width_for_drag(20, 3, 1440, m), 120);
        // Dragged right past the grid's minimum: it stops leaving the grid its 200.
        assert_eq!(tree_width_for_drag(1439, 3, 1440, m), 1440 - 6 - 200);
    }

    /// Dragging the window narrower than both minimums must not produce a negative pane, which
    /// `SetWindowPos` can't express and which would make the grid disappear rather than shrink.
    #[test]
    fn a_window_too_narrow_for_both_minimums_still_lays_out() {
        let m = metrics();
        for width in [0, 1, 50, 200, 206, 320] {
            let tree = clamp_tree_width(240, width, m);
            assert!(tree >= 0, "tree {tree} at width {width}");
            let l = layout(width, 400, tree, m);
            for rect in [l.tree, l.splitter, l.grid, l.status_bar] {
                assert!(
                    rect.width >= 0 && rect.height >= 0,
                    "{rect:?} at width {width}"
                );
            }
            assert_eq!(l.tree.width + l.splitter.width + l.grid.width, width);
        }
        // The grid keeps its minimum for as long as there is room for it, and the tree pays.
        assert_eq!(clamp_tree_width(240, 300, m), 94);
        assert_eq!(clamp_tree_width(240, 100, m), 0);
    }

    /// A window shorter than the status bar is a degenerate case a resize animation can pass
    /// through, and a negative content height would make every pane invisible until the next
    /// layout.
    #[test]
    fn a_window_shorter_than_the_status_bar_still_lays_out() {
        let m = metrics();
        let l = layout(800, 10, 240, m);
        assert_eq!(l.status_bar.height, 10);
        assert_eq!(l.tree.height, 0);
        assert_eq!(l.grid.height, 0);
        assert_eq!(l.status_bar.y, 0);
    }

    /// The default is a compromise, so pin it: on the narrowest laptop Prvw supports the grid
    /// still gets more room than the tree.
    #[test]
    fn the_default_tree_width_leaves_the_grid_the_larger_half() {
        let m = metrics();
        let tree = clamp_tree_width(TREE_PANE_DEFAULT, 1024, m);
        let l = layout(1024, 700, tree, m);
        assert_eq!(tree, TREE_PANE_DEFAULT);
        assert!(l.grid.width > l.tree.width);
    }
}
