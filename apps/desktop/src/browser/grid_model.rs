//! Pure, headless-testable model behind the browse-mode thumbnail grid.
//!
//! The `NSCollectionView` data source (`grid.rs`) is the macOS view wiring; this module holds the
//! platform-free decisions it leans on so they're unit-testable without AppKit:
//!
//! - The **folder image list**: the selected folder's supported images, sorted via
//!   [`crate::navigation::SortBy`] (default by name) — the same ordering the image-mode
//!   `DirectoryList` uses, so opening a grid item lands on the matching index.
//! - The **selected index** with clamping, and the **empty detection** that drives the
//!   "(No images)" overlay + the grid-non-focusable rule.
//! - A **folder generation** counter so completions (thumbnails) from a folder the user has since
//!   navigated away from are dropped (mirrors `previews::State::folder_generation`).
//!
//! The model never touches AppKit and never reads the disk on the main thread — the folder listing
//! itself runs on the shared `folder_scan` worker and is handed in via [`GridModel::set_images`].

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::navigation::SortBy;

/// The grid's headless state: the listed images (sorted), the selected index, and a generation
/// counter for stale-completion checks.
pub struct GridModel {
    /// The folder's supported images, sorted by `sort_by`. Empty when the folder has none.
    images: Vec<PathBuf>,
    /// Sort order applied to `images`. Default: name.
    sort_by: SortBy,
    /// The selected grid index, or `None` when the grid is empty / nothing picked yet.
    selected: Option<usize>,
    /// Bumped on every `set_images`. A thumbnail completion captured an earlier generation; the
    /// main thread drops it if the generation no longer matches (the user moved to a new folder).
    generation: u64,
}

impl GridModel {
    #[must_use]
    pub fn new(sort_by: SortBy) -> Self {
        Self {
            images: Vec::new(),
            sort_by,
            selected: None,
            generation: 0,
        }
    }

    /// Replace the listed images with the (already-collected) `images`, sorted by the model's
    /// current `sort_by`. Resets the selection to the first image (or `None` when empty) and bumps
    /// the generation so older thumbnail completions are dropped. Returns the new generation so the
    /// caller can stamp fresh thumbnail requests with it.
    pub fn set_images(&mut self, mut images: Vec<PathBuf>) -> u64 {
        crate::navigation::sort::sort_files(&mut images, self.sort_by);
        self.selected = if images.is_empty() { None } else { Some(0) };
        self.images = images;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// The current folder generation. Thumbnail requests carry it; completions whose generation no
    /// longer matches are stale and dropped.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The number of images in the grid.
    #[must_use]
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// True when the listed folder has no supported images. Drives the "(No images)" overlay and
    /// the grid-non-focusable rule (Tab skips an empty grid).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// The path at `index`, if in range.
    #[must_use]
    pub fn path(&self, index: usize) -> Option<&Path> {
        self.images.get(index).map(PathBuf::as_path)
    }

    /// All image paths, in display order. Used to hand the folder to image mode on open.
    #[must_use]
    pub fn images(&self) -> &[PathBuf] {
        &self.images
    }

    /// The selected grid index, or `None` when the grid is empty.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The selected image's path, if any.
    #[must_use]
    pub fn selected_path(&self) -> Option<&Path> {
        self.selected.and_then(|i| self.path(i))
    }

    /// Set the selected index, clamped to `[0, len)`. A request to select in an empty grid leaves
    /// the selection `None`. Returns the resulting selection.
    pub fn set_selected(&mut self, index: usize) -> Option<usize> {
        if self.images.is_empty() {
            self.selected = None;
        } else {
            self.selected = Some(index.min(self.images.len() - 1));
        }
        self.selected
    }

    /// Replace the listed images from a live folder re-scan, **preserving the selection by path**
    /// (not by index). Unlike [`set_images`](Self::set_images) — which resets the selection to the
    /// first image (used on a fresh folder pick) — this keeps the grid cursor on the same file
    /// across adds/removes, or lands sensibly when the selected file was deleted: the next
    /// surviving image, the new last if it was last, or `None` for an emptied folder. Bumps the
    /// generation so stale thumbnail completions are dropped. Returns the new generation.
    ///
    /// `selected_path` is the path the grid was sitting on before the re-scan (its
    /// [`selected_path`](Self::selected_path)); the new selection is computed by the pure
    /// [`select_after_rescan`].
    pub fn apply_rescan(&mut self, mut images: Vec<PathBuf>, selected_path: Option<&Path>) -> u64 {
        crate::navigation::sort::sort_files(&mut images, self.sort_by);
        self.selected = select_after_rescan(&self.images, &images, selected_path);
        self.images = images;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }
}

/// Where the grid selection should land after a live folder re-scan, tracking the previously
/// selected file by path. Pure so the next/prev/empty decision is unit-testable without AppKit.
///
/// - The selected file still exists → stay on it (its new sorted index).
/// - It was deleted but images remain → land on the next surviving image after it in the OLD
///   order (the file that took its place), or the new last image if it was the last one.
/// - The folder is now empty → `None`.
/// - Nothing was selected before → the first image (or `None` when empty), matching a fresh listing.
///
/// `old` and `new` are both sorted in the active order; `selected_path` is the file the grid was on.
#[must_use]
pub fn select_after_rescan(
    old: &[PathBuf],
    new: &[PathBuf],
    selected_path: Option<&Path>,
) -> Option<usize> {
    if new.is_empty() {
        return None;
    }
    let Some(selected) = selected_path else {
        // No prior selection (a fresh/empty grid that just gained images) → first image.
        return Some(0);
    };
    // Still present → track it by path.
    if let Some(index) = new.iter().position(|p| p.as_path() == selected) {
        return Some(index);
    }
    // Deleted. Walk forward from its old position to the first surviving successor; that's the
    // image that took its slot. If none survive (it was last, or all successors went too), land on
    // the new last image — the "previous" relative to where the cursor was.
    if let Some(old_index) = old.iter().position(|p| p.as_path() == selected) {
        for succ in &old[old_index + 1..] {
            if let Some(idx) = new.iter().position(|p| p == succ) {
                return Some(idx);
            }
        }
    }
    Some(new.len() - 1)
}

/// Clamp a half-open visible `range` to `[0, len)`, returning an empty range when `len` is 0.
/// The `NSCollectionView` reports visible index paths that can momentarily run past the item count
/// during layout; both the scheduler and the cache expect a clamped range, so normalize here once.
#[must_use]
pub fn clamp_visible_range(range: Range<usize>, len: usize) -> Range<usize> {
    if len == 0 {
        return 0..0;
    }
    let start = range.start.min(len);
    let end = range.end.clamp(start, len);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(format!("/folder/{name}"))
    }

    #[test]
    fn set_images_sorts_and_selects_first() {
        let mut m = GridModel::new(SortBy::Name);
        let generation = m.set_images(vec![p("c.jpg"), p("a.jpg"), p("b.jpg")]);
        assert_eq!(generation, 1);
        assert_eq!(m.len(), 3);
        assert!(!m.is_empty());
        // Sorted by name.
        assert_eq!(m.path(0), Some(p("a.jpg").as_path()));
        assert_eq!(m.path(1), Some(p("b.jpg").as_path()));
        assert_eq!(m.path(2), Some(p("c.jpg").as_path()));
        // First image preselected.
        assert_eq!(m.selected(), Some(0));
        assert_eq!(m.selected_path(), Some(p("a.jpg").as_path()));
    }

    #[test]
    fn set_images_bumps_generation_each_time() {
        let mut m = GridModel::new(SortBy::Name);
        assert_eq!(m.set_images(vec![p("x.jpg")]), 1);
        assert_eq!(m.set_images(vec![p("y.jpg")]), 2);
        assert_eq!(m.generation(), 2);
    }

    #[test]
    fn empty_folder_has_no_selection() {
        let mut m = GridModel::new(SortBy::Name);
        m.set_images(Vec::new());
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
        assert_eq!(m.selected(), None);
        assert_eq!(m.selected_path(), None);
        // Selecting in an empty grid stays None.
        assert_eq!(m.set_selected(3), None);
    }

    #[test]
    fn set_selected_clamps_to_range() {
        let mut m = GridModel::new(SortBy::Name);
        m.set_images(vec![p("a.jpg"), p("b.jpg")]);
        assert_eq!(m.set_selected(0), Some(0));
        assert_eq!(m.set_selected(1), Some(1));
        // Past the end clamps to the last image.
        assert_eq!(m.set_selected(99), Some(1));
    }

    #[test]
    fn set_images_resets_selection_to_first() {
        let mut m = GridModel::new(SortBy::Name);
        m.set_images(vec![p("a.jpg"), p("b.jpg")]);
        m.set_selected(1);
        assert_eq!(m.selected(), Some(1));
        // A new folder resets the selection.
        m.set_images(vec![p("x.jpg")]);
        assert_eq!(m.selected(), Some(0));
    }

    #[test]
    fn clamp_visible_range_handles_overshoot_and_empty() {
        // Normal range, in bounds.
        assert_eq!(clamp_visible_range(2..5, 10), 2..5);
        // Overshooting end clamps to len.
        assert_eq!(clamp_visible_range(8..20, 10), 8..10);
        // Start past len collapses to an empty range at len.
        assert_eq!(clamp_visible_range(15..20, 10), 10..10);
        // Empty folder → empty range.
        assert_eq!(clamp_visible_range(0..5, 0), 0..0);
    }

    #[test]
    fn apply_rescan_keeps_selection_on_the_same_file_across_an_add() {
        let mut m = GridModel::new(SortBy::Name);
        m.set_images(vec![p("a.jpg"), p("c.jpg")]);
        m.set_selected(1); // on "c"
        let selected = m.selected_path().map(std::path::Path::to_path_buf);
        // "b" added; the selection stays on "c", which shifts from index 1 to 2.
        let generation = m.apply_rescan(
            vec![p("a.jpg"), p("b.jpg"), p("c.jpg")],
            selected.as_deref(),
        );
        assert_eq!(generation, 2);
        assert_eq!(m.selected_path(), Some(p("c.jpg").as_path()));
        assert_eq!(m.selected(), Some(2));
    }

    #[test]
    fn apply_rescan_keeps_selection_when_a_non_selected_file_is_removed() {
        let mut m = GridModel::new(SortBy::Name);
        m.set_images(vec![p("a.jpg"), p("b.jpg"), p("c.jpg")]);
        m.set_selected(2); // on "c"
        let selected = m.selected_path().map(std::path::Path::to_path_buf);
        // "a" removed; "c" survives, now at index 1.
        m.apply_rescan(vec![p("b.jpg"), p("c.jpg")], selected.as_deref());
        assert_eq!(m.selected(), Some(1));
        assert_eq!(m.selected_path(), Some(p("c.jpg").as_path()));
    }

    #[test]
    fn select_after_rescan_deleted_selected_lands_on_next_then_previous_then_empty() {
        let old = vec![p("a.jpg"), p("b.jpg"), p("c.jpg")];
        // Selected "b" deleted → next surviving is "c", now index 1.
        assert_eq!(
            select_after_rescan(&old, &[p("a.jpg"), p("c.jpg")], Some(p("b.jpg").as_path())),
            Some(1)
        );
        // Selected "c" (last) deleted → new last "b", index 1.
        assert_eq!(
            select_after_rescan(&old, &[p("a.jpg"), p("b.jpg")], Some(p("c.jpg").as_path())),
            Some(1)
        );
        // Selected "b" AND its successor "c" both gone → new last "a", index 0.
        assert_eq!(
            select_after_rescan(&old, &[p("a.jpg")], Some(p("b.jpg").as_path())),
            Some(0)
        );
        // Folder emptied → no selection.
        assert_eq!(
            select_after_rescan(&old, &[], Some(p("b.jpg").as_path())),
            None
        );
    }

    #[test]
    fn select_after_rescan_no_prior_selection_picks_first_or_none() {
        // Empty grid gained images → first.
        assert_eq!(select_after_rescan(&[], &[p("a.jpg")], None), Some(0));
        // Still empty → none.
        assert_eq!(select_after_rescan(&[], &[], None), None);
    }

    #[test]
    fn images_returns_sorted_slice() {
        let mut m = GridModel::new(SortBy::Name);
        m.set_images(vec![p("b.jpg"), p("a.jpg")]);
        let imgs = m.images();
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0], p("a.jpg"));
        assert_eq!(imgs[1], p("b.jpg"));
    }
}
