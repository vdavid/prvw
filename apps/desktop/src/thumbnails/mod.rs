//! Thumbnail preload: generate thumbs for every file in the folder so a
//! navigation to any index can render a blurry placeholder instantly
//! while the full decode runs.
//!
//! ## Overview
//!
//! Uses macOS's system-wide QuickLook thumbnail cache (shared with Finder,
//! Preview, and every other Mac app) rather than maintaining our own
//! on-disk store. `quicklookd` handles generation and caching; we just
//! submit requests. The cache key includes the file's mtime, so modified
//! files invalidate automatically.
//!
//! ## Flow
//!
//! 1. On navigation, `App` calls [`State::set_folder`] with every path.
//! 2. A [`scheduler::Scheduler`] orders indices centered-outward, with
//!    indices outside the full-decode preload window (`|i − current| > 2`)
//!    prioritized first.
//! 3. The app loop drains the scheduler via [`State::drain_ready_to_submit`]
//!    each tick and fires QL requests via [`quicklook::RequestTable`].
//! 4. `quicklookd` completions arrive on our main thread as
//!    `AppCommand::ThumbnailReady` / `ThumbnailFailed` events (via
//!    `EventLoopProxy::send_event`, which `winit` routes through
//!    `user_event`).
//! 5. `App` stores the RGBA8 thumb in the cache and calls back into
//!    [`State::mark_ready`] so the scheduler moves on.
//!
//! ## Pause semantics
//!
//! When a primary decode is pending (the user navigated to an uncached
//! index), the scheduler is paused so it doesn't compete for I/O or
//! shared system CPU. `quicklookd` runs out-of-process so our thread
//! isn't directly at risk, but the courtesy still matters.

pub mod metadata;
#[cfg(target_os = "macos")]
pub mod quicklook;
pub mod scheduler;

pub use scheduler::{RequestId, Status};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A ready thumbnail stored in the cache.
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Source image dimensions (EXIF-oriented), from `metadata::read_dimensions`.
    /// Available even before the thumb arrives — used for auto-fit window.
    pub source_width: u32,
    pub source_height: u32,
}

/// All thumbnail-related state owned by `App`.
pub struct State {
    pub scheduler: scheduler::Scheduler,
    /// Ready thumbnails, keyed by folder index.
    pub cache: HashMap<usize, Thumbnail>,
    /// Source image dimensions per index, read lazily via ImageIO.
    /// Populated on first access and retained for the folder's lifetime.
    pub source_dims: HashMap<usize, metadata::Dimensions>,
    /// Folder paths, kept so the app loop can look up paths by index
    /// when draining the scheduler.
    pub paths: Vec<PathBuf>,
    /// Monotonic counter bumped on every `set_folder`. Completion blocks
    /// capture the value at submit-time; the main thread drops completions
    /// whose generation no longer matches, so a thumb for a stale folder
    /// can never be inserted into the new folder's cache at a wrong index.
    pub folder_generation: u64,
    #[cfg(target_os = "macos")]
    pub requests: quicklook::RequestTable,
}

impl State {
    pub fn new() -> Self {
        // Half the cores, floor 1. Out-of-process quicklookd does the
        // real work, so this cap is about I/O + system courtesy.
        let max_parallel = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).max(1))
            .unwrap_or(4);
        Self {
            scheduler: scheduler::Scheduler::new(max_parallel),
            cache: HashMap::new(),
            source_dims: HashMap::new(),
            paths: Vec::new(),
            folder_generation: 0,
            #[cfg(target_os = "macos")]
            requests: quicklook::RequestTable::new(),
        }
    }

    pub fn generation(&self) -> u64 {
        self.folder_generation
    }

    /// Reset for a new folder. Cancels in-flight requests, clears the
    /// cache, and reseeds the scheduler. Bumps `folder_generation` so any
    /// still-pending completions fire with a stale generation and get
    /// dropped by the executor.
    pub fn set_folder(&mut self, paths: Vec<PathBuf>, current: usize) {
        #[cfg(target_os = "macos")]
        self.requests.cancel_all();
        self.cache.clear();
        self.source_dims.clear();
        let len = paths.len();
        self.paths = paths;
        self.folder_generation = self.folder_generation.wrapping_add(1);
        self.scheduler.set_folder(len, current);
    }

    pub fn set_current(&mut self, current: usize) {
        self.scheduler.set_current(current);
    }

    pub fn pause(&mut self) {
        self.scheduler.pause();
    }

    pub fn resume(&mut self) {
        self.scheduler.resume();
    }

    pub fn get(&self, index: usize) -> Option<&Thumbnail> {
        self.cache.get(&index)
    }

    /// Look up (or lazily read) the source pixel dimensions for `index`.
    pub fn source_dimensions(&mut self, index: usize) -> Option<metadata::Dimensions> {
        if let Some(dims) = self.source_dims.get(&index) {
            return Some(*dims);
        }
        let path = self.paths.get(index)?;
        let dims = metadata::read_dimensions(path)?;
        self.source_dims.insert(index, dims);
        Some(dims)
    }

    /// Called when `quicklookd` hands back a thumbnail. Stores in cache
    /// and lets the scheduler know the slot is free.
    pub fn mark_ready(
        &mut self,
        index: usize,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        request_id: RequestId,
    ) {
        #[cfg(target_os = "macos")]
        self.requests.forget(request_id);
        #[cfg(not(target_os = "macos"))]
        let _ = request_id;

        // Read source dims if we haven't yet. Cheap (<1ms) and the auto-
        // fit path needs them.
        let source = self
            .source_dimensions(index)
            .unwrap_or(metadata::Dimensions { width, height });
        self.cache.insert(
            index,
            Thumbnail {
                width,
                height,
                rgba,
                source_width: source.width,
                source_height: source.height,
            },
        );
        self.scheduler.mark_ready(index);
    }

    pub fn mark_failed(&mut self, index: usize, request_id: RequestId) {
        #[cfg(target_os = "macos")]
        self.requests.forget(request_id);
        #[cfg(not(target_os = "macos"))]
        let _ = request_id;
        self.scheduler.mark_failed(index);
    }

    /// Return the path for an index, if valid.
    pub fn path(&self, index: usize) -> Option<&Path> {
        self.paths.get(index).map(|p| p.as_path())
    }

    pub fn status(&self) -> Status {
        self.scheduler.status()
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}
