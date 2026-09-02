use crate::navigation::sort::{SortBy, sort_files};
use crate::paths;
use std::path::{Path, PathBuf};

/// Tracks the list of image files in a directory and the current position.
pub struct DirectoryList {
    files: Vec<PathBuf>,
    current_index: usize,
    sort_by: SortBy,
}

impl DirectoryList {
    /// A one-file stand-in for the folder, installed at launch (and on an in-app open) so the
    /// opened image can paint before the folder has been read. `read_dir` on a big network folder
    /// takes tens of seconds; blocking the window on it is what this exists to avoid.
    ///
    /// While it's in place, `navigation::State::scan_pending` names the folder being scanned and
    /// navigation stays put — there's nowhere to go in a list of one. The real list replaces it
    /// via [`from_scan`](Self::from_scan) when `AppCommand::FolderScanned` lands.
    #[must_use]
    pub fn provisional(file_path: &Path, sort_by: SortBy) -> Self {
        Self {
            files: vec![file_path.to_path_buf()],
            current_index: 0,
            sort_by,
        }
    }

    /// Build the real folder list from a finished scan, sorted, positioned at `current` — matched
    /// **by path**, the same invariant `set_sort_by` and the live-sync re-scan keep, so the picture
    /// on screen stays the picture on screen.
    ///
    /// `None` when `current` isn't in the listing (deleted while the scan ran). The caller then
    /// keeps the provisional list and stays put rather than yanking the user to some other image.
    ///
    /// The match is on the **file name**, not the whole path. Every entry came from one `read_dir`
    /// of the folder `current` lives in, so no two of them can share a name, while the two paths
    /// can easily be two spellings of one folder: the launch argument as the user typed it against
    /// the canonical form a scan request carried. `paths::same_file_name` settles the name under
    /// the host's own rules.
    #[must_use]
    pub fn from_scan(mut images: Vec<PathBuf>, sort_by: SortBy, current: &Path) -> Option<Self> {
        sort_files(&mut images, sort_by);
        let current_index = images
            .iter()
            .position(|p| paths::same_file_name(p, current))?;
        Some(Self {
            files: images,
            current_index,
            sort_by,
        })
    }

    /// Create a navigation list from an explicit set of files (a multi-file open, a folder
    /// played as a list, a grid reveal), positioned at `current`.
    ///
    /// The list sorts into the user's order, so the image the caller opens is rarely the one at
    /// index 0: `prvw b.png a.png` opens b.png and sorts the list a, b. Positioning at the named
    /// file is what keeps the index, the title, and the pixels on screen talking about the same
    /// image. Pass `None` to start at the first file (a folder played from the top); a path
    /// that isn't in the list starts there too.
    pub fn from_explicit(mut files: Vec<PathBuf>, sort_by: SortBy, current: Option<&Path>) -> Self {
        sort_files(&mut files, sort_by);
        let current_index = current
            .and_then(|target| files.iter().position(|f| paths::same_path(f, target)))
            .unwrap_or(0);
        log::info!("Using explicit file list: {} images", files.len());
        if let Some(file) = files.get(current_index) {
            log::debug!(
                "Current position: [{current_index}] {}",
                file.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        Self {
            files,
            current_index,
            sort_by,
        }
    }

    /// Rebuild from an already-sorted file list, positioned at `current_index`. Used by live
    /// folder sync: the `folder_diff` re-scan produces the new sorted list and the index to land
    /// on (current-by-path, or the next/previous after a delete-current), so this just installs
    /// them. `current_index` is clamped to a valid slot; callers never pass an out-of-range index
    /// for a non-empty list, but the clamp keeps a bug from panicking on `current()`.
    pub fn from_sorted(files: Vec<PathBuf>, sort_by: SortBy, current_index: usize) -> Self {
        let current_index = current_index.min(files.len().saturating_sub(1));
        Self {
            files,
            current_index,
            sort_by,
        }
    }

    /// Re-sort by a new column. Preserves the current image by tracking its
    /// path across the re-sort. Idempotent.
    pub fn set_sort_by(&mut self, sort_by: SortBy) {
        if self.sort_by == sort_by {
            return;
        }
        if self.files.is_empty() {
            self.sort_by = sort_by;
            return;
        }
        let current_path = self.files[self.current_index].clone();
        sort_files(&mut self.files, sort_by);
        self.current_index = self
            .files
            .iter()
            .position(|p| p == &current_path)
            .unwrap_or(0);
        self.sort_by = sort_by;
    }

    pub fn sort_by(&self) -> SortBy {
        self.sort_by
    }

    pub fn current(&self) -> &Path {
        &self.files[self.current_index]
    }

    pub fn current_index(&self) -> usize {
        self.current_index
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Move by a signed delta. Returns the net movement (may differ from
    /// `delta` at list boundaries when not wrapping).
    ///
    /// - `loop_on = false`: clamped to `[0, len - 1]`. Net movement may be
    ///   smaller than `delta` at the edges.
    /// - `loop_on = true`: wraps modulo `len`. Net movement always equals
    ///   `delta` (modulo the directory size).
    ///
    /// Used by both single-step navigation (delta = ±1) and the debounced
    /// path (any coalesced delta).
    pub fn go_by(&mut self, delta: i32, loop_on: bool) -> i32 {
        if delta == 0 || self.files.is_empty() {
            return 0;
        }
        let old = self.current_index as i64;
        let len = self.files.len() as i64;
        let new = if loop_on {
            ((old + delta as i64) % len + len) % len
        } else {
            (old + delta as i64).clamp(0, len - 1)
        };
        self.current_index = new as usize;
        (new - old) as i32
    }

    /// Jump to the first image. Returns the net movement (always negative
    /// or zero), or `0` when already at the first image / the directory is
    /// empty. Mirrors `go_by`'s "no movement" return convention so callers
    /// can short-circuit the post-navigation work on a no-op.
    pub fn go_to_first(&mut self) -> i32 {
        if self.files.is_empty() || self.current_index == 0 {
            return 0;
        }
        let net = -(self.current_index as i32);
        self.current_index = 0;
        net
    }

    /// Jump to the last image. Returns the net movement (always positive
    /// or zero), or `0` when already at the last image / the directory is
    /// empty. Mirrors `go_by`'s "no movement" return convention.
    pub fn go_to_last(&mut self) -> i32 {
        if self.files.is_empty() {
            return 0;
        }
        let last = self.files.len() - 1;
        if self.current_index == last {
            return 0;
        }
        let net = (last as i32) - (self.current_index as i32);
        self.current_index = last;
        net
    }

    /// Get the file at a specific index (for preloader lookups).
    pub fn get(&self, index: usize) -> Option<&Path> {
        self.files.get(index).map(|p| p.as_path())
    }

    /// Return a clone of every path in the folder. Used by the preview
    /// scheduler to get a stable list of files to generate previews for.
    pub fn files(&self) -> Vec<PathBuf> {
        self.files.clone()
    }

    /// Borrow the file list. Used by callers that want to translate paths
    /// back into directory indices (for example, the cache→indices
    /// projection in `SharedAppState`) without paying for a clone.
    pub fn files_ref(&self) -> &[PathBuf] {
        &self.files
    }

    /// Return indices of files to preload, ordered by priority (most
    /// likely next first). `count` controls how many indices ahead and
    /// behind current to include. The highest-priority index comes first
    /// so callers can submit with FIFO scheduling for the preloader.
    ///
    /// Ordering rules:
    /// - `Direction::Forward`  → `[N+1, N+2, … , N-1, N-2, …]`
    /// - `Direction::Backward` → `[N-1, N-2, … , N+1, N+2, …]`
    /// - `Direction::Unknown`  → interleaved `[N+1, N-1, N+2, N-2, …]`
    ///
    /// When `loop_on` is true, indices wrap around the directory boundary
    /// so the preload window stays full at the edges. Duplicates (which
    /// can appear when the directory is smaller than `2*count + 1`) are
    /// dropped, preserving the priority order of the first occurrence.
    pub fn preload_range(&self, count: usize, direction: Direction, loop_on: bool) -> Vec<usize> {
        if count == 0 {
            return Vec::new();
        }
        let total = self.files.len();
        if total == 0 {
            return Vec::new();
        }
        let cur = self.current_index;
        let mut indices = Vec::with_capacity(count * 2);

        let ahead = |step: usize| -> Option<usize> {
            let raw = cur.checked_add(step)?;
            if raw < total {
                Some(raw)
            } else if loop_on {
                Some(raw % total)
            } else {
                None
            }
        };
        let behind = |step: usize| -> Option<usize> {
            if cur >= step {
                Some(cur - step)
            } else if loop_on {
                let total_i = total as isize;
                let raw = cur as isize - step as isize;
                let wrapped = ((raw % total_i) + total_i) % total_i;
                Some(wrapped as usize)
            } else {
                None
            }
        };

        match direction {
            Direction::Forward => {
                for step in 1..=count {
                    if let Some(i) = ahead(step) {
                        indices.push(i);
                    }
                }
                for step in 1..=count {
                    if let Some(i) = behind(step) {
                        indices.push(i);
                    }
                }
            }
            Direction::Backward => {
                for step in 1..=count {
                    if let Some(i) = behind(step) {
                        indices.push(i);
                    }
                }
                for step in 1..=count {
                    if let Some(i) = ahead(step) {
                        indices.push(i);
                    }
                }
            }
            Direction::Unknown => {
                for step in 1..=count {
                    if let Some(i) = ahead(step) {
                        indices.push(i);
                    }
                    if let Some(i) = behind(step) {
                        indices.push(i);
                    }
                }
            }
        }

        // Drop duplicates while preserving first-seen priority order, and
        // also drop the current index (the caller never wants to preload it).
        let mut seen = vec![false; total];
        seen[cur] = true;
        indices.retain(|&i| {
            if seen[i] {
                false
            } else {
                seen[i] = true;
                true
            }
        });
        indices
    }
}

/// Direction hint for `preload_range`. Set by the last navigation — the
/// preloader prioritizes the direction the user is moving.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
    /// Before the first navigation, or after a non-directional jump (e.g.
    /// open-file). Interleaves ahead/behind so both sides are warmed.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build the list the way the app does: hand `from_scan` the folder's images (as a scan would)
    /// and the image being opened.
    fn list_at(dir: &tempfile::TempDir, name: &str) -> DirectoryList {
        let images: Vec<PathBuf> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(crate::decoding::is_supported_extension)
            })
            .collect();
        DirectoryList::from_scan(images, SortBy::Name, &dir.path().join(name)).unwrap()
    }

    fn create_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        // Create some fake image files (they won't decode, but directory scanning doesn't decode)
        for name in [
            "apple.jpg",
            "Banana.png",
            "cherry.gif",
            "date.webp",
            "readme.txt",
            "fig.BMP",
        ] {
            fs::write(dir.path().join(name), b"fake").unwrap();
        }
        dir
    }

    #[test]
    fn explicit_list_sorts_but_sits_on_the_file_that_was_named() {
        // `prvw b.png a.png`: the list sorts a, b, and the current position is b — the file the
        // user named. Index and displayed image agreeing is what keeps the title honest and the
        // cache keyed to the right slot.
        let files = vec![PathBuf::from("b.png"), PathBuf::from("a.png")];
        let list = DirectoryList::from_explicit(files, SortBy::Name, Some(Path::new("b.png")));
        assert_eq!(list.current(), Path::new("b.png"));
        assert_eq!(list.current_index(), 1);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn explicit_list_starts_at_the_top_without_a_named_file() {
        // A folder played as a list names no file, so it opens at the first image in sort order.
        // A path that isn't in the list lands there too, rather than panicking or hunting.
        let files = vec![PathBuf::from("b.png"), PathBuf::from("a.png")];
        let from_top = DirectoryList::from_explicit(files.clone(), SortBy::Name, None);
        assert_eq!(from_top.current(), Path::new("a.png"));
        let stranger =
            DirectoryList::from_explicit(files, SortBy::Name, Some(Path::new("elsewhere.png")));
        assert_eq!(stranger.current(), Path::new("a.png"));
    }

    #[test]
    fn scans_and_sorts_case_insensitive() {
        let dir = create_test_dir();
        let list = list_at(&dir, "cherry.gif");

        // Should have 5 image files (not readme.txt), sorted case-insensitive
        assert_eq!(list.len(), 5);
        let names: Vec<_> = list
            .files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "apple.jpg",
                "Banana.png",
                "cherry.gif",
                "date.webp",
                "fig.BMP"
            ]
        );
    }

    #[test]
    fn tracks_current_position() {
        let dir = create_test_dir();
        let list = list_at(&dir, "cherry.gif");
        assert_eq!(
            list.current().file_name().unwrap().to_str().unwrap(),
            "cherry.gif"
        );
        assert_eq!(list.current_index(), 2);
    }

    /// A folder lists its own entries, links included: the viewer stays on the link the user
    /// opened rather than jumping to the file it points at.
    #[test]
    #[cfg(unix)]
    fn a_symlink_into_another_folder_still_lands_on_the_link() {
        let real = create_test_dir();
        let links = tempfile::tempdir().unwrap();
        // One image of its own, plus a link named later in sort order than that one.
        std::fs::write(links.path().join("apple.jpg"), b"x").unwrap();
        let link = links.path().join("zebra.gif");
        std::os::unix::fs::symlink(real.path().join("cherry.gif"), &link).unwrap();

        let list = list_at(&links, "zebra.gif");
        assert_eq!(list.len(), 2);
        assert_eq!(list.current(), link, "the link is where the viewer is");
        assert_eq!(list.current_index(), 1);
    }

    #[test]
    fn go_by_clamps_and_returns_net_delta() {
        let dir = create_test_dir();
        let mut list = list_at(&dir, "cherry.gif"); // index 2 of 5
        assert_eq!(list.go_by(0, false), 0);
        assert_eq!(list.current_index(), 2);
        assert_eq!(list.go_by(2, false), 2); // 2 -> 4
        assert_eq!(list.current_index(), 4);
        assert_eq!(list.go_by(5, false), 0); // clamped at end
        assert_eq!(list.current_index(), 4);
        assert_eq!(list.go_by(-10, false), -4); // clamped at start
        assert_eq!(list.current_index(), 0);
    }

    #[test]
    fn go_by_wraps_when_loop_on() {
        // 5 images, current = 4 (last). Next wraps to 0.
        let dir = create_test_dir();
        let mut list = list_at(&dir, "fig.BMP");
        assert_eq!(list.current_index(), 4);
        let net = list.go_by(1, true);
        assert_eq!(net, -4, "net movement is -4 when wrapping from 4 to 0");
        assert_eq!(list.current_index(), 0);

        // Previous from 0 wraps to last.
        let net = list.go_by(-1, true);
        assert_eq!(net, 4);
        assert_eq!(list.current_index(), 4);

        // Big jump wraps modularly.
        list.go_by(-4, true); // back to 0
        assert_eq!(list.current_index(), 0);
        list.go_by(7, true); // 0 + 7 mod 5 = 2
        assert_eq!(list.current_index(), 2);
    }

    #[test]
    fn navigation_at_boundaries() {
        let dir = create_test_dir();
        let mut list = list_at(&dir, "apple.jpg");
        assert_eq!(list.current_index(), 0);

        // Can't go before first
        assert_eq!(list.go_by(-1, false), 0);
        assert_eq!(list.current_index(), 0);

        // Can go forward
        assert_eq!(list.go_by(1, false), 1);
        assert_eq!(list.current_index(), 1);

        // Go to last
        while list.go_by(1, false) != 0 {}
        assert_eq!(list.current_index(), 4);

        // Can't go past last
        assert_eq!(list.go_by(1, false), 0);
        assert_eq!(list.current_index(), 4);
    }

    #[test]
    fn preload_range_at_edges() {
        let dir = create_test_dir();
        let list = list_at(&dir, "apple.jpg");

        // At index 0, forward preload should go [1, 2] (nothing before).
        assert_eq!(list.preload_range(2, Direction::Forward, false), vec![1, 2]);
        assert_eq!(
            list.preload_range(2, Direction::Backward, false),
            vec![1, 2]
        );
        assert_eq!(list.preload_range(2, Direction::Unknown, false), vec![1, 2]);
    }

    #[test]
    fn preload_range_forward_priority() {
        // At index 2 of 5 with count=2, forward order is [N+1, N+2, N-1, N-2]
        // = [3, 4, 1, 0]. The highest-priority slot (first) goes to N+1.
        let dir = create_test_dir();
        let list = list_at(&dir, "cherry.gif");
        assert_eq!(
            list.preload_range(2, Direction::Forward, false),
            vec![3, 4, 1, 0]
        );
    }

    #[test]
    fn preload_range_backward_priority() {
        // Backward flips the ordering: [N-1, N-2, N+1, N+2] = [1, 0, 3, 4].
        let dir = create_test_dir();
        let list = list_at(&dir, "cherry.gif");
        assert_eq!(
            list.preload_range(2, Direction::Backward, false),
            vec![1, 0, 3, 4]
        );
    }

    #[test]
    fn preload_range_unknown_interleaves() {
        // Unknown interleaves both sides: [N+1, N-1, N+2, N-2] = [3, 1, 4, 0].
        let dir = create_test_dir();
        let list = list_at(&dir, "cherry.gif");
        assert_eq!(
            list.preload_range(2, Direction::Unknown, false),
            vec![3, 1, 4, 0]
        );
    }

    #[test]
    fn preload_range_wraps_when_loop_on() {
        // 5 images, current = 4 (last), count = 2, forward + loop on:
        // ahead wraps to [0, 1]; behind = [3, 2].
        let dir = create_test_dir();
        let list = list_at(&dir, "fig.BMP");
        assert_eq!(list.current_index(), 4);
        assert_eq!(
            list.preload_range(2, Direction::Forward, true),
            vec![0, 1, 3, 2]
        );
    }

    #[test]
    fn preload_range_dedupes_in_small_directory() {
        // 5 images, current = 2, count = 5, loop on. Without dedup we'd
        // see indices repeat (and the current index in the list).
        let dir = create_test_dir();
        let list = list_at(&dir, "cherry.gif");
        let indices = list.preload_range(5, Direction::Unknown, true);
        let mut sorted = indices.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 3, 4]); // 2 (current) excluded
        assert!(!indices.contains(&2), "current index never in preload list");
    }

    #[test]
    fn go_to_first_jumps_from_middle() {
        let dir = create_test_dir();
        let mut list = list_at(&dir, "cherry.gif"); // index 2 of 5
        assert_eq!(list.current_index(), 2);
        assert_eq!(list.go_to_first(), -2);
        assert_eq!(list.current_index(), 0);
    }

    #[test]
    fn go_to_first_at_first_is_noop() {
        let dir = create_test_dir();
        let mut list = list_at(&dir, "apple.jpg"); // index 0
        assert_eq!(list.current_index(), 0);
        assert_eq!(list.go_to_first(), 0);
        assert_eq!(list.current_index(), 0);
    }

    #[test]
    fn go_to_last_jumps_from_middle() {
        let dir = create_test_dir();
        let mut list = list_at(&dir, "cherry.gif"); // index 2 of 5
        assert_eq!(list.current_index(), 2);
        assert_eq!(list.go_to_last(), 2);
        assert_eq!(list.current_index(), 4);
    }

    #[test]
    fn go_to_last_at_last_is_noop() {
        let dir = create_test_dir();
        let mut list = list_at(&dir, "fig.BMP"); // index 4 (last)
        assert_eq!(list.current_index(), 4);
        assert_eq!(list.go_to_last(), 0);
        assert_eq!(list.current_index(), 4);
    }

    #[test]
    fn a_provisional_list_holds_just_the_opened_file() {
        // Launch installs this before the folder scan finishes, so the image can paint while a
        // slow share is still being read.
        let dir = create_test_dir();
        let target = dir.path().join("cherry.gif");
        let mut list = DirectoryList::provisional(&target, SortBy::Name);
        assert_eq!(list.len(), 1);
        assert_eq!(list.current_index(), 0);
        assert_eq!(list.current(), target);
        assert_eq!(list.go_by(1, false), 0, "nowhere to go in a list of one");
        assert_eq!(list.go_by(-1, true), 0, "not even with loop navigation on");
    }

    #[test]
    fn from_scan_lands_on_the_open_image_by_path() {
        // The scan lands: the real list replaces the provisional one and keeps showing the same
        // picture, now at its true position in the folder.
        let dir = create_test_dir();
        let current = dir.path().join("cherry.gif");
        let scanned = vec![
            dir.path().join("date.webp"),
            dir.path().join("apple.jpg"),
            current.clone(),
            dir.path().join("Banana.png"),
        ];
        let list = DirectoryList::from_scan(scanned, SortBy::Name, &current).unwrap();
        assert_eq!(list.len(), 4);
        assert_eq!(list.current(), current, "same picture, real position");
        assert_eq!(list.current_index(), 2, "apple, Banana, cherry, date");
    }

    #[test]
    fn from_scan_gives_up_when_the_open_image_is_gone() {
        // Deleted while the scan ran. The caller keeps the provisional list so the picture on
        // screen stays put rather than jumping to a stranger.
        let dir = create_test_dir();
        let scanned = vec![dir.path().join("apple.jpg")];
        assert!(
            DirectoryList::from_scan(scanned, SortBy::Name, &dir.path().join("cherry.gif"))
                .is_none()
        );
    }

    #[test]
    fn scanning_a_folder_with_no_images_gives_no_list() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readme.txt"), b"not an image").unwrap();
        assert!(
            DirectoryList::from_scan(Vec::new(), SortBy::Name, &dir.path().join("readme.txt"))
                .is_none()
        );
    }
}
