use crate::image_loader;
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
                    .is_some_and(image_loader::is_supported_extension)
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

    pub fn current(&self) -> &Path {
        &self.files[self.current_index]
    }

    pub fn current_index(&self) -> usize {
        self.current_index
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Move to the next image. Returns true if the position changed.
    pub fn go_next(&mut self) -> bool {
        if self.current_index + 1 < self.files.len() {
            self.current_index += 1;
            true
        } else {
            false
        }
    }

    /// Move to the previous image. Returns true if the position changed.
    pub fn go_prev(&mut self) -> bool {
        if self.current_index > 0 {
            self.current_index -= 1;
            true
        } else {
            false
        }
    }

    /// Get the file at a specific index (for preloader lookups).
    pub fn get(&self, index: usize) -> Option<&Path> {
        self.files.get(index).map(|p| p.as_path())
    }

    /// Return indices of files to preload: up to `count` ahead and `count` behind current.
    pub fn preload_range(&self, count: usize) -> Vec<usize> {
        let mut indices = Vec::new();
        let start = self.current_index.saturating_sub(count);
        let end = (self.current_index + count + 1).min(self.files.len());
        for i in start..end {
            if i != self.current_index {
                indices.push(i);
            }
        }
        indices
    }
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
    fn navigation_at_boundaries() {
        let dir = create_test_dir();
        let target = dir.path().join("apple.jpg");
        let mut list = DirectoryList::from_file(&target).unwrap();
        assert_eq!(list.current_index(), 0);

        // Can't go before first
        assert!(!list.go_prev());
        assert_eq!(list.current_index(), 0);

        // Can go forward
        assert!(list.go_next());
        assert_eq!(list.current_index(), 1);

        // Go to last
        while list.go_next() {}
        assert_eq!(list.current_index(), 4);

        // Can't go past last
        assert!(!list.go_next());
        assert_eq!(list.current_index(), 4);
    }

    #[test]
    fn preload_range_at_edges() {
        let dir = create_test_dir();
        let target = dir.path().join("apple.jpg");
        let list = DirectoryList::from_file(&target).unwrap();

        // At index 0, preload_range(2) should return indices 1, 2 (nothing before)
        let range = list.preload_range(2);
        assert_eq!(range, vec![1, 2]);
    }

    #[test]
    fn preload_range_in_middle() {
        let dir = create_test_dir();
        let target = dir.path().join("cherry.gif");
        let list = DirectoryList::from_file(&target).unwrap();

        // At index 2, preload_range(2) should return 0, 1, 3, 4
        let range = list.preload_range(2);
        assert_eq!(range, vec![0, 1, 3, 4]);
    }

    #[test]
    fn empty_directory_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readme.txt"), b"not an image").unwrap();
        let result = DirectoryList::from_file(&dir.path().join("readme.txt"));
        assert!(result.is_none());
    }
}
