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

pub mod dim_prefetch;
pub mod metadata;
#[cfg(target_os = "macos")]
pub mod quicklook;
pub mod scheduler;

pub use scheduler::{RequestId, Status};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Distance from `current` beyond which cached thumbnails are evicted
/// from RAM. Slightly larger than `scheduler::WINDOW_RADIUS` so a small
/// nav doesn't immediately evict thumbs we just generated. For 10 000
/// images at ~3 MB per RGBA8 thumb (1024 × 768), this caps the cache at
/// ~1.2 GB peak. Matches `scheduler::WINDOW_RADIUS` × 4 — gives the user
/// a couple of nav jumps' worth of headroom before evicting.
pub const RETENTION_RADIUS: usize = 200;

/// A ready thumbnail stored in the cache. Just the pixels — source
/// dimensions are read lazily via `State::source_dimensions(index)` at
/// display time. Caching them here was a footgun: storing them eagerly
/// in `mark_ready` issues an ImageIO read per thumb, which blocks the
/// main thread for hundreds of milliseconds per file on network shares
/// and stalled the *initial* image render for 10+ seconds. Lazy reads
/// happen only for the index we're actually displaying.
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// All thumbnail-related state owned by `App`.
pub struct State {
    pub scheduler: scheduler::Scheduler,
    /// Ready thumbnails, keyed by folder index.
    pub cache: HashMap<usize, Thumbnail>,
    /// Parallel pixel-dimension prefetcher. Populates dims for the
    /// active window in the background; lazy-fallback on miss.
    pub dim_prefetcher: dim_prefetch::DimPrefetcher,
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
            dim_prefetcher: dim_prefetch::DimPrefetcher::new(),
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
        self.dim_prefetcher.reset();
        let len = paths.len();
        self.paths = paths;
        self.folder_generation = self.folder_generation.wrapping_add(1);
        self.scheduler.set_folder(len, current);
        self.enqueue_dim_prefetch_window(current);
    }

    pub fn set_current(&mut self, current: usize) {
        self.scheduler.set_current(current);
        self.evict_distant_thumbs(current);
        self.enqueue_dim_prefetch_window(current);
    }

    /// Push pixel-dimension prefetch jobs for every index in
    /// `current ± WINDOW_RADIUS` not already cached. The 16-thread
    /// worker pool drains these in parallel.
    fn enqueue_dim_prefetch_window(&self, current: usize) {
        let radius = scheduler::WINDOW_RADIUS as isize;
        let cur = current as isize;
        let len = self.paths.len() as isize;
        let lo = (cur - radius).max(0);
        let hi = (cur + radius).min(len - 1);
        for idx in lo..=hi {
            let idx = idx as usize;
            if self.dim_prefetcher.get(idx).is_some() {
                continue;
            }
            if let Some(path) = self.paths.get(idx) {
                self.dim_prefetcher.enqueue(idx, path.clone());
            }
        }
    }

    /// Drop cached thumbnails whose distance from `current` exceeds
    /// [`RETENTION_RADIUS`]. Keeps RAM bounded for big folders. Also
    /// removes the corresponding entries from the scheduler's `cached`
    /// set so that re-visiting that area later re-enqueues them.
    fn evict_distant_thumbs(&mut self, current: usize) {
        let cur = current as isize;
        let radius = RETENTION_RADIUS as isize;
        let evicted: Vec<usize> = self
            .cache
            .keys()
            .copied()
            .filter(|&i| (i as isize - cur).unsigned_abs() as isize > radius)
            .collect();
        if evicted.is_empty() {
            return;
        }
        for i in &evicted {
            self.cache.remove(i);
            // Tell the scheduler we no longer have them cached, so they
            // become eligible to be re-queued if the user navigates back.
            self.scheduler.uncache(*i);
        }
        // Drop dim cache for evicted indices too — distant images don't
        // need their dims warm.
        self.dim_prefetcher.invalidate(&evicted);
        log::debug!(
            "Evicted {} thumb(s) outside retention radius {} of current {current}",
            evicted.len(),
            RETENTION_RADIUS
        );
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

    /// Look up the source pixel dimensions for `index`. Hot path is the
    /// prefetcher cache (filled in parallel by the 16-thread pool while
    /// the user wasn't yet looking at this image). Slow fallback is a
    /// synchronous read on the main thread — only fires if the user
    /// out-paces the prefetcher.
    pub fn source_dimensions(&mut self, index: usize) -> Option<metadata::Dimensions> {
        if let Some(dims) = self.dim_prefetcher.get(index) {
            return Some(dims);
        }
        // Lazy synchronous fallback. Uses the same three-tier dispatcher
        // the prefetcher pool uses, so per-format optimisations apply
        // here too. Insert into the prefetcher cache so subsequent
        // lookups for this index are instant.
        let path = self.paths.get(index)?;
        let dims = metadata::read_dimensions_fast(path)?;
        self.dim_prefetcher.put(index, dims);
        Some(dims)
    }

    /// Called when `quicklookd` hands back a thumbnail. Stores in cache
    /// and lets the scheduler know the slot is free.
    ///
    /// **Does NOT pre-read source dimensions.** That's lazy via
    /// `source_dimensions(index)` from `display_thumbnail_placeholder`,
    /// which only fires for the user's actual nav target. Pre-reading
    /// here for all 38 cached thumbs was a 7+ second main-thread block
    /// on network shares (ImageIO file-header read per file).
    pub fn mark_ready(
        &mut self,
        index: usize,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        _request_id: RequestId,
    ) {
        // The worker thread cleans up its `entries` map by handling
        // `WorkerMsg::Forget` fired from the completion block — main
        // thread doesn't need to forget here.
        self.cache.insert(
            index,
            Thumbnail {
                width,
                height,
                rgba,
            },
        );
        self.scheduler.mark_ready(index);
    }

    pub fn mark_failed(&mut self, index: usize, _request_id: RequestId) {
        // Worker handles its own entries cleanup via Forget message.
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
