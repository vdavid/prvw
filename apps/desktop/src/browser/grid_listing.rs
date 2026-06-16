//! Background folder-image listing for the browse-mode grid.
//!
//! Selecting a folder in the tree must list that folder's supported images for the grid, but
//! reading a directory on the main thread freezes winit's event loop whenever the filesystem is
//! slow — a stale SMB mount blocks for ~10 s. So listing runs on a dedicated background worker
//! (`std::thread` + `mpsc`, the same pattern as `navigation::preloader` and `browser::TreeScanner`
//! — no tokio). The worker reads the directory off-thread and posts the image paths back to the
//! main thread via the global `EventLoopProxy` as `AppCommand::BrowseFolderListed`, where the
//! executor populates the grid model and reloads the collection view.
//!
//! This also subsumes the old main-thread `count_supported_images` read the tree-selection handler
//! used to do: the listing returns the actual paths, and the count is just their length.

use std::path::PathBuf;
use std::sync::mpsc;

/// A background folder lister. Owns one `std::thread` that lists a folder's supported images and
/// posts them back to the main thread. One request supersedes the previous (the worker drains the
/// channel and only acts on the newest path) so a burst of fast tree-arrow presses doesn't queue a
/// listing per folder — only the folder the user landed on gets listed. The folder generation in
/// the grid model drops any stale completion that still arrives.
pub struct FolderLister {
    request_tx: mpsc::Sender<PathBuf>,
}

impl FolderLister {
    /// Spawn the lister worker. It runs until the `Sender` (held by the grid, alive for the
    /// window's life) drops, closing the channel and ending the loop.
    pub fn start() -> Self {
        let (request_tx, request_rx) = mpsc::channel::<PathBuf>();
        std::thread::Builder::new()
            .name("prvw-grid-list".into())
            .spawn(move || {
                while let Ok(mut path) = request_rx.recv() {
                    // Coalesce: if newer requests are already queued, skip ahead to the newest so a
                    // fast scroll through folders only lists the one the user settled on.
                    while let Ok(newer) = request_rx.try_recv() {
                        path = newer;
                    }
                    let images = super::grid_listing::list_supported_images(&path);
                    log::debug!(
                        "Grid folder listed: {} ({} image(s))",
                        path.display(),
                        images.len()
                    );
                    // Post back to the main thread. If the proxy is gone the app is shutting down
                    // and we just drop the work.
                    crate::commands::send_command(
                        crate::commands::AppCommand::BrowseFolderListed {
                            folder: path,
                            images,
                        },
                    );
                }
                log::debug!("Grid folder lister worker exiting");
            })
            .expect("Failed to spawn grid folder lister worker thread");
        log::info!("Grid folder lister started (dedicated OS thread)");
        FolderLister { request_tx }
    }

    /// Enqueue a folder listing. Fire-and-forget; the result comes back as
    /// `AppCommand::BrowseFolderListed`.
    pub fn list(&self, folder: PathBuf) {
        if self.request_tx.send(folder).is_err() {
            log::warn!("Grid folder lister worker is gone — dropping listing request");
        }
    }
}

/// List the supported image files directly inside `folder` (non-recursive), unsorted.
///
/// Reuses `decoding::is_supported_extension` so the list tracks exactly what the viewer can open.
/// The grid model sorts the result via `navigation::SortBy`; sorting here would duplicate that and
/// lose the model's sort setting. Returns an empty `Vec` for an unreadable folder (the grid then
/// shows its "(No images)" empty state rather than erroring).
#[must_use]
pub fn list_supported_images(folder: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(crate::decoding::is_supported_extension)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lists_only_supported_images_non_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for name in ["a.jpg", "b.PNG", "c.webp", "readme.txt", "data.json"] {
            fs::write(root.join(name), b"x").unwrap();
        }
        fs::create_dir(root.join("subdir")).unwrap();
        fs::write(root.join("subdir").join("nested.jpg"), b"x").unwrap();

        let mut names: Vec<String> = list_supported_images(root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        // 3 images (jpg, png case-insensitive, webp); txt/json and the subdir's nested image
        // (non-recursive) don't count.
        assert_eq!(names, vec!["a.jpg", "b.PNG", "c.webp"]);
    }

    #[test]
    fn unreadable_folder_is_empty_not_error() {
        assert!(list_supported_images(std::path::Path::new("/no/such/folder/prvw")).is_empty());
    }
}
