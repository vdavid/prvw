//! Pure folder-diff logic for live folder sync.
//!
//! Given the old image list, a freshly-scanned list, the active `SortBy`, and the currently
//! displayed image's path, [`diff_folder`] computes what changed: which paths were added, which
//! were removed, and — when the current image was removed — where navigation should land (the next
//! image, the previous if it was last, or the empty state if the folder is now imageless).
//!
//! Kept pure (no filesystem, no `App`) so the add/remove/reorder cases under each `SortBy` and the
//! delete-current → next/prev/empty decision are unit-testable without real timing or I/O. The
//! caller ([`App::execute_command`]'s `FolderChanged` arm) does the I/O (the off-thread re-scan)
//! and applies the result to the live `DirectoryList`.

use crate::navigation::sort::{SortBy, sort_files};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What navigation should do with the current image after a re-scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentOutcome {
    /// The current image still exists. Stay on it; its index in the new sorted list is `index`
    /// (the path-tracked re-anchor — the existing "track current by path across re-sorts"
    /// behavior). Added/removed siblings shift around it.
    Unchanged { index: usize },
    /// The current image was removed and the folder still has images. Navigate to `index` in the
    /// new sorted list — the image that took the removed one's slot (its successor), or the new
    /// last image if the removed one was last.
    Navigate { index: usize },
    /// The current image was removed and the folder has no images left. Enter the image-mode
    /// "(No images)" empty state.
    Empty,
}

/// The result of diffing the old image list against a fresh re-scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderDiff {
    /// The freshly-scanned list, sorted by the active `SortBy`. This becomes the new
    /// `DirectoryList` backing.
    pub sorted: Vec<PathBuf>,
    /// Paths present in the new list but not the old one.
    pub added: Vec<PathBuf>,
    /// Paths present in the old list but not the new one.
    pub removed: Vec<PathBuf>,
    /// What to do with the current image.
    pub current: CurrentOutcome,
}

impl FolderDiff {
    /// True when nothing about the list changed (no adds, no removes). The caller can skip applying
    /// in that case — a `Modify`-only change still wants cache eviction but no list mutation.
    pub fn list_unchanged(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Diff `old` against `scanned` under `sort_by`, tracking `current` (the displayed image's path).
///
/// `scanned` is the raw, unsorted re-scan of the folder; this sorts it with the same comparator
/// the `DirectoryList` uses, so indices map 1:1 to the live list. `current` is `None` when nothing
/// is displayed (already in the empty state) — then `CurrentOutcome` is `Empty` if the folder is
/// imageless, else `Navigate` to index 0 (a folder that just gained images while empty).
pub fn diff_folder(
    old: &[PathBuf],
    scanned: Vec<PathBuf>,
    sort_by: SortBy,
    current: Option<&Path>,
) -> FolderDiff {
    let mut sorted = scanned;
    sort_files(&mut sorted, sort_by);

    let old_set: HashSet<&PathBuf> = old.iter().collect();
    let new_set: HashSet<&PathBuf> = sorted.iter().collect();

    let added: Vec<PathBuf> = sorted
        .iter()
        .filter(|p| !old_set.contains(*p))
        .cloned()
        .collect();
    let removed: Vec<PathBuf> = old
        .iter()
        .filter(|p| !new_set.contains(*p))
        .cloned()
        .collect();

    let current = compute_current_outcome(old, &sorted, current);

    FolderDiff {
        sorted,
        added,
        removed,
        current,
    }
}

/// Decide where the current image lands after the re-scan.
fn compute_current_outcome(
    old: &[PathBuf],
    sorted: &[PathBuf],
    current: Option<&Path>,
) -> CurrentOutcome {
    let Some(current) = current else {
        // Nothing was displayed (empty state). A folder that gained images opens the first one.
        return if sorted.is_empty() {
            CurrentOutcome::Empty
        } else {
            CurrentOutcome::Navigate { index: 0 }
        };
    };

    // Still present → stay on it (track by path).
    if let Some(index) = sorted.iter().position(|p| p == current) {
        return CurrentOutcome::Unchanged { index };
    }

    // Removed. Empty folder → empty state.
    if sorted.is_empty() {
        return CurrentOutcome::Empty;
    }

    // Removed but images remain: land on the image that took the removed one's place. Use the
    // removed image's position in the OLD list to choose its successor — the image that was just
    // after it (now occupying a nearby slot in the new sorted list), or the previous if it was
    // last. This matches the spec: "navigate to the next image, or the previous if it was last".
    let old_index = old.iter().position(|p| p == current);
    match old_index {
        Some(i) if i + 1 < old.len() => {
            // There was a "next" in the old list. Find where that successor sits now; if it too was
            // removed, walk forward to the first old-successor that survives.
            for succ in &old[i + 1..] {
                if let Some(idx) = sorted.iter().position(|p| p == succ) {
                    return CurrentOutcome::Navigate { index: idx };
                }
            }
            // All successors gone too — fall back to the new last image.
            CurrentOutcome::Navigate {
                index: sorted.len() - 1,
            }
        }
        // It was last (or not found in old) — land on the new last image (the "previous").
        _ => CurrentOutcome::Navigate {
            index: sorted.len() - 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn paths(ss: &[&str]) -> Vec<PathBuf> {
        ss.iter().map(|s| p(s)).collect()
    }

    #[test]
    fn no_change_reports_unchanged_current() {
        let old = paths(&["/d/a.jpg", "/d/b.jpg", "/d/c.jpg"]);
        let diff = diff_folder(
            &old,
            paths(&["/d/c.jpg", "/d/a.jpg", "/d/b.jpg"]), // unsorted re-scan
            SortBy::Name,
            Some(&p("/d/b.jpg")),
        );
        assert!(diff.list_unchanged());
        assert_eq!(diff.current, CurrentOutcome::Unchanged { index: 1 });
        assert_eq!(diff.sorted, old);
    }

    #[test]
    fn added_image_inserts_at_sorted_position_and_keeps_current_by_path() {
        let old = paths(&["/d/a.jpg", "/d/c.jpg"]);
        // "b" added; current is "c" which shifts from index 1 to index 2.
        let diff = diff_folder(
            &old,
            paths(&["/d/a.jpg", "/d/b.jpg", "/d/c.jpg"]),
            SortBy::Name,
            Some(&p("/d/c.jpg")),
        );
        assert_eq!(diff.added, paths(&["/d/b.jpg"]));
        assert!(diff.removed.is_empty());
        assert_eq!(diff.sorted, paths(&["/d/a.jpg", "/d/b.jpg", "/d/c.jpg"]));
        assert_eq!(diff.current, CurrentOutcome::Unchanged { index: 2 });
    }

    #[test]
    fn removed_non_current_drops_it_and_keeps_current() {
        let old = paths(&["/d/a.jpg", "/d/b.jpg", "/d/c.jpg"]);
        // "a" removed; current is "c", now at index 1.
        let diff = diff_folder(
            &old,
            paths(&["/d/b.jpg", "/d/c.jpg"]),
            SortBy::Name,
            Some(&p("/d/c.jpg")),
        );
        assert_eq!(diff.removed, paths(&["/d/a.jpg"]));
        assert!(diff.added.is_empty());
        assert_eq!(diff.current, CurrentOutcome::Unchanged { index: 1 });
    }

    #[test]
    fn removed_current_middle_navigates_to_next() {
        let old = paths(&["/d/a.jpg", "/d/b.jpg", "/d/c.jpg"]);
        // current "b" removed; "next" was "c", now at index 1.
        let diff = diff_folder(
            &old,
            paths(&["/d/a.jpg", "/d/c.jpg"]),
            SortBy::Name,
            Some(&p("/d/b.jpg")),
        );
        assert_eq!(diff.removed, paths(&["/d/b.jpg"]));
        assert_eq!(diff.current, CurrentOutcome::Navigate { index: 1 });
        assert_eq!(diff.sorted[1], p("/d/c.jpg"));
    }

    #[test]
    fn removed_current_last_navigates_to_previous() {
        let old = paths(&["/d/a.jpg", "/d/b.jpg", "/d/c.jpg"]);
        // current "c" (last) removed → land on the new last, "b" at index 1.
        let diff = diff_folder(
            &old,
            paths(&["/d/a.jpg", "/d/b.jpg"]),
            SortBy::Name,
            Some(&p("/d/c.jpg")),
        );
        assert_eq!(diff.current, CurrentOutcome::Navigate { index: 1 });
        assert_eq!(diff.sorted[1], p("/d/b.jpg"));
    }

    #[test]
    fn removed_current_only_image_goes_empty() {
        let old = paths(&["/d/only.jpg"]);
        let diff = diff_folder(&old, paths(&[]), SortBy::Name, Some(&p("/d/only.jpg")));
        assert_eq!(diff.removed, paths(&["/d/only.jpg"]));
        assert_eq!(diff.current, CurrentOutcome::Empty);
    }

    #[test]
    fn removed_current_and_its_successor_walks_to_next_surviving() {
        let old = paths(&["/d/a.jpg", "/d/b.jpg", "/d/c.jpg", "/d/d.jpg"]);
        // current "b" and its successor "c" both removed in one batch → land on "d".
        let diff = diff_folder(
            &old,
            paths(&["/d/a.jpg", "/d/d.jpg"]),
            SortBy::Name,
            Some(&p("/d/b.jpg")),
        );
        assert_eq!(diff.current, CurrentOutcome::Navigate { index: 1 });
        assert_eq!(diff.sorted[1], p("/d/d.jpg"));
    }

    #[test]
    fn empty_state_stays_empty_when_no_images_arrive() {
        let diff = diff_folder(&[], paths(&[]), SortBy::Name, None);
        assert_eq!(diff.current, CurrentOutcome::Empty);
    }

    #[test]
    fn empty_state_opens_first_when_an_image_appears() {
        let diff = diff_folder(&[], paths(&["/d/a.jpg"]), SortBy::Name, None);
        assert_eq!(diff.added, paths(&["/d/a.jpg"]));
        assert_eq!(diff.current, CurrentOutcome::Navigate { index: 0 });
    }

    #[test]
    fn date_sort_orders_the_new_list_by_date_comparator() {
        // diff_folder must sort with the active SortBy, not assume Name. We can't set mtimes on
        // bare PathBufs, but FileType is a deterministic non-Name comparator we can assert on.
        let old = paths(&["/d/a.png", "/d/b.jpg"]);
        // FileType: jpg < png, so the sorted order is [b.jpg, a.png]. Add c.jpg.
        let diff = diff_folder(
            &old,
            paths(&["/d/a.png", "/d/b.jpg", "/d/c.jpg"]),
            SortBy::FileType,
            Some(&p("/d/a.png")),
        );
        assert_eq!(
            diff.sorted,
            paths(&["/d/b.jpg", "/d/c.jpg", "/d/a.png"]),
            "list must be sorted by the active SortBy (FileType)"
        );
        // current "a.png" is now last under FileType.
        assert_eq!(diff.current, CurrentOutcome::Unchanged { index: 2 });
    }
}
