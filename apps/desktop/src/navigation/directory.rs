use crate::decoding;
use std::path::{Path, PathBuf};

/// Tracks the list of image files in a directory and the current position.
pub struct DirectoryList {
    files: Vec<PathBuf>,
    current_index: usize,
}

impl DirectoryList {
    /// Scan the parent directory of `file_path` for image files.
    /// Returns None if the parent directory can't be read or contains no images.
    pub fn from_file(file_path: &Path) -> Option<Self> {
        let dir = file_path.parent()?;
        let canonical_target = file_path.canonicalize().ok()?;

        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(decoding::is_supported_extension)
            })
            .collect();

        // Sort case-insensitive by filename
        files.sort_by(|a, b| {
            let name_a = a
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            let name_b = b
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            name_a.cmp(&name_b)
        });

        if files.is_empty() {
            return None;
        }

        let current_index = files
            .iter()
            .position(|f| f.canonicalize().ok().as_ref() == Some(&canonical_target))
            .unwrap_or(0);

        log::info!(
            "Scanned directory: {} images in {}",
            files.len(),
            dir.display()
        );
        log::debug!(
            "Current position: [{current_index}] {}",
            files[current_index]
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );

        Some(Self {
            files,
            current_index,
        })
    }

    /// Create a navigation list from an explicit set of files (multi-select open).
    /// The first file in the list is the initial image.
    pub fn from_explicit(files: Vec<PathBuf>) -> Self {
        log::info!("Using explicit file list: {} images", files.len());
        if let Some(first) = files.first() {
            log::debug!(
                "Current position: [0] {}",
                first.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        Self {
            files,
            current_index: 0,
        }
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

    /// Move by a signed delta, clamped to `[0, len - 1]`. Returns the net
    /// movement (may be smaller than `delta` at list boundaries). Used by
    /// both single-step navigation (delta = ±1) and the debounced path
    /// (any coalesced delta).
    pub fn go_by(&mut self, delta: i32) -> i32 {
        if delta == 0 || self.files.is_empty() {
            return 0;
        }
        let old = self.current_index as i64;
        let max = self.files.len() as i64 - 1;
        let new = (old + delta as i64).clamp(0, max);
        self.current_index = new as usize;
        (new - old) as i32
    }

    /// Get the file at a specific index (for preloader lookups).
    pub fn get(&self, index: usize) -> Option<&Path> {
        self.files.get(index).map(|p| p.as_path())
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
    pub fn preload_range(&self, count: usize, direction: Direction) -> Vec<usize> {
        if count == 0 {
            return Vec::new();
        }
        let total = self.files.len();
        let cur = self.current_index;
        let mut indices = Vec::with_capacity(count * 2);

        let ahead = |step: usize| cur.checked_add(step).filter(|&i| i < total);
        let behind = |step: usize| if cur >= step { Some(cur - step) } else { None };

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
    fn scans_and_sorts_case_insensitive() {
        let dir = create_test_dir();
        let target = dir.path().join("cherry.gif");
        let list = DirectoryList::from_file(&target).unwrap();

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
        let target = dir.path().join("cherry.gif");
        let list = DirectoryList::from_file(&target).unwrap();
        assert_eq!(
            list.current().file_name().unwrap().to_str().unwrap(),
            "cherry.gif"
        );
        assert_eq!(list.current_index(), 2);
    }

    #[test]
    fn go_by_clamps_and_returns_net_delta() {
        let dir = create_test_dir();
        let target = dir.path().join("cherry.gif"); // index 2 of 5
        let mut list = DirectoryList::from_file(&target).unwrap();
        assert_eq!(list.go_by(0), 0);
        assert_eq!(list.current_index(), 2);
        assert_eq!(list.go_by(2), 2); // 2 -> 4
        assert_eq!(list.current_index(), 4);
        assert_eq!(list.go_by(5), 0); // clamped at end
        assert_eq!(list.current_index(), 4);
        assert_eq!(list.go_by(-10), -4); // clamped at start
        assert_eq!(list.current_index(), 0);
    }

    #[test]
    fn navigation_at_boundaries() {
        let dir = create_test_dir();
        let target = dir.path().join("apple.jpg");
        let mut list = DirectoryList::from_file(&target).unwrap();
        assert_eq!(list.current_index(), 0);

        // Can't go before first
        assert_eq!(list.go_by(-1), 0);
        assert_eq!(list.current_index(), 0);

        // Can go forward
        assert_eq!(list.go_by(1), 1);
        assert_eq!(list.current_index(), 1);

        // Go to last
        while list.go_by(1) != 0 {}
        assert_eq!(list.current_index(), 4);

        // Can't go past last
        assert_eq!(list.go_by(1), 0);
        assert_eq!(list.current_index(), 4);
    }

    #[test]
    fn preload_range_at_edges() {
        let dir = create_test_dir();
        let target = dir.path().join("apple.jpg");
        let list = DirectoryList::from_file(&target).unwrap();

        // At index 0, forward preload should go [1, 2] (nothing before).
        assert_eq!(list.preload_range(2, Direction::Forward), vec![1, 2]);
        assert_eq!(list.preload_range(2, Direction::Backward), vec![1, 2]);
        assert_eq!(list.preload_range(2, Direction::Unknown), vec![1, 2]);
    }

    #[test]
    fn preload_range_forward_priority() {
        // At index 2 of 5 with count=2, forward order is [N+1, N+2, N-1, N-2]
        // = [3, 4, 1, 0]. The highest-priority slot (first) goes to N+1.
        let dir = create_test_dir();
        let target = dir.path().join("cherry.gif");
        let list = DirectoryList::from_file(&target).unwrap();
        assert_eq!(list.preload_range(2, Direction::Forward), vec![3, 4, 1, 0]);
    }

    #[test]
    fn preload_range_backward_priority() {
        // Backward flips the ordering: [N-1, N-2, N+1, N+2] = [1, 0, 3, 4].
        let dir = create_test_dir();
        let target = dir.path().join("cherry.gif");
        let list = DirectoryList::from_file(&target).unwrap();
        assert_eq!(list.preload_range(2, Direction::Backward), vec![1, 0, 3, 4]);
    }

    #[test]
    fn preload_range_unknown_interleaves() {
        // Unknown interleaves both sides: [N+1, N-1, N+2, N-2] = [3, 1, 4, 0].
        let dir = create_test_dir();
        let target = dir.path().join("cherry.gif");
        let list = DirectoryList::from_file(&target).unwrap();
        assert_eq!(list.preload_range(2, Direction::Unknown), vec![3, 1, 4, 0]);
    }

    #[test]
    fn empty_directory_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readme.txt"), b"not an image").unwrap();
        let result = DirectoryList::from_file(&dir.path().join("readme.txt"));
        assert!(result.is_none());
    }
}
