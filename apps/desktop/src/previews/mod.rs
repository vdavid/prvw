//! Preview preload: generate previews for every file in the folder so a
//! navigation to any index can render a blurry placeholder instantly
//! while the full decode runs.
//!
//! ## Overview
//!
//! Uses macOS's system-wide QuickLook preview cache (shared with Finder,
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
//!    `AppCommand::PreviewReady` / `PreviewFailed` events (via
//!    `EventLoopProxy::send_event`, which `winit` routes through
//!    `user_event`).
//! 5. `App` stores the RGBA8 preview in the cache and calls back into
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
use std::sync::OnceLock;

/// Rough size of one cached preview (~1024² RGBA8). Used only to derive the
/// generation radius from the byte budget; real eviction uses exact
/// `rgba.len()` per preview (they're aspect-fit, so most are a bit smaller).
const EST_PREVIEW_BYTES: usize = 1024 * 1024 * 4;

/// Floor and ceiling for the RAM-scaled preview cache budget. The floor
/// keeps a small machine usable (a handful of neighbor placeholders); the
/// ceiling stops a 256 GB Mac Pro from spending 2 GB on previews.
const MIN_PREVIEW_BUDGET: usize = 64 * 1024 * 1024;
const MAX_PREVIEW_BUDGET: usize = 1024 * 1024 * 1024;

/// RAM-proportional preview cache budget: 1/128 of physical RAM, clamped to
/// `[MIN, MAX]`. 64 GB → 512 MB, 16 GB → 128 MB, 8 GB → 64 MB (floor). Bytes,
/// not a fixed preview count, so it self-adjusts to preview size and
/// display DPI. Queried once (RAM doesn't change at runtime).
pub fn preview_budget_bytes() -> usize {
    static BUDGET: OnceLock<usize> = OnceLock::new();
    *BUDGET.get_or_init(|| budget_for_ram(crate::platform::total_physical_ram_bytes() as usize))
}

/// Pure budget math, split out for testing without depending on host RAM.
fn budget_for_ram(ram_bytes: usize) -> usize {
    (ram_bytes / 128).clamp(MIN_PREVIEW_BUDGET, MAX_PREVIEW_BUDGET)
}

/// Generation radius derived from the budget, capped at
/// [`scheduler::WINDOW_RADIUS`]. We never generate more than the byte-budgeted
/// cache will retain — otherwise quicklookd would churn producing previews we
/// evict on arrival. At the 512 MB budget this lands at the full 50; at a
/// 128 MB budget (~16 GB machine) it's ~16; at the 64 MB floor it's ~8.
pub fn generation_radius() -> usize {
    generation_radius_for_budget(preview_budget_bytes())
}

/// Pure generation-radius math, split out for testing.
fn generation_radius_for_budget(budget: usize) -> usize {
    (budget / (2 * EST_PREVIEW_BYTES)).clamp(2, scheduler::WINDOW_RADIUS)
}

/// A ready preview stored in the cache. Just the pixels — source
/// dimensions are read lazily via `State::source_dimensions(index)` at
/// display time. Caching them here was a footgun: storing them eagerly
/// in `mark_ready` issues an ImageIO read per preview, which blocks the
/// main thread for hundreds of milliseconds per file on network shares
/// and stalled the *initial* image render for 10+ seconds. Lazy reads
/// happen only for the index we're actually displaying.
pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// All preview-related state owned by `App`.
pub struct State {
    pub scheduler: scheduler::Scheduler,
    /// Ready previews, keyed by folder index.
    pub cache: HashMap<usize, Preview>,
    /// Parallel pixel-dimension prefetcher. Populates dims for the
    /// active window in the background; lazy-fallback on miss.
    pub dim_prefetcher: dim_prefetch::DimPrefetcher,
    /// Folder paths, kept so the app loop can look up paths by index
    /// when draining the scheduler.
    pub paths: Vec<PathBuf>,
    /// Current navigation index. Tracked here (mirrors the scheduler's) so
    /// byte-budget eviction can measure distance-from-current when a preview
    /// arrives, not just on `set_current`.
    current: usize,
    /// Monotonic counter bumped on every `set_folder`. Completion blocks
    /// capture the value at submit-time; the main thread drops completions
    /// whose generation no longer matches, so a preview for a stale folder
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
            scheduler: scheduler::Scheduler::new(max_parallel)
                .with_window_radius(generation_radius()),
            cache: HashMap::new(),
            dim_prefetcher: dim_prefetch::DimPrefetcher::new(),
            paths: Vec::new(),
            current: 0,
            folder_generation: 0,
            #[cfg(target_os = "macos")]
            requests: quicklook::RequestTable::new(
                || crate::commands::AppCommand::PreviewsAvailable,
                "prvw-previewgen",
            ),
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
        self.current = current;
        self.folder_generation = self.folder_generation.wrapping_add(1);
        self.scheduler.set_folder(len, current);
        self.enqueue_dim_prefetch_window(current);
    }

    pub fn set_current(&mut self, current: usize) {
        self.current = current;
        self.scheduler.set_current(current);
        self.evict_to_budget(current, preview_budget_bytes());
        self.enqueue_dim_prefetch_window(current);
    }

    /// Push pixel-dimension prefetch jobs for every index in the generation
    /// window (`current ± window_radius`) not already cached. The 16-thread
    /// worker pool drains these in parallel.
    fn enqueue_dim_prefetch_window(&self, current: usize) {
        let radius = self.scheduler.window_radius() as isize;
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

    /// Total bytes held by cached previews. For diagnostics / RSS
    /// attribution.
    pub fn memory_bytes(&self) -> usize {
        self.cache.values().map(|t| t.rgba.len()).sum()
    }

    /// Evict cached previews, farthest-from-`current` first, until total
    /// bytes fit within `budget`. Keeps the previews most likely to be
    /// navigated to (nearest the current image) — so we never pin a stale
    /// trail from where the user *was*. Also drops each evicted index from
    /// the scheduler's `cached` set so re-entering that area re-enqueues it,
    /// and from the dim cache. `budget` is a parameter (not read inline) so
    /// tests can exercise the policy without depending on the host's RAM.
    fn evict_to_budget(&mut self, current: usize, budget: usize) {
        let mut total = self.memory_bytes();
        if total <= budget {
            return;
        }
        let cur = current as isize;
        // Farthest from current first.
        let mut by_distance: Vec<usize> = self.cache.keys().copied().collect();
        by_distance.sort_by_key(|&i| std::cmp::Reverse((i as isize - cur).unsigned_abs()));

        let mut evicted: Vec<usize> = Vec::new();
        for i in by_distance {
            if total <= budget {
                break;
            }
            if let Some(preview) = self.cache.remove(&i) {
                total -= preview.rgba.len();
                // Let the scheduler re-queue it if the user navigates back.
                self.scheduler.uncache(i);
                evicted.push(i);
            }
        }
        if !evicted.is_empty() {
            self.dim_prefetcher.invalidate(&evicted);
            log::debug!(
                "Evicted {} preview(s) to fit {} KB budget around current {current} ({} KB resident)",
                evicted.len(),
                budget / 1024,
                total / 1024,
            );
        }
    }

    pub fn pause(&mut self) {
        self.scheduler.pause();
    }

    pub fn resume(&mut self) {
        self.scheduler.resume();
    }

    pub fn get(&self, index: usize) -> Option<&Preview> {
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

    /// Called when `quicklookd` hands back a preview. Stores in cache
    /// and lets the scheduler know the slot is free.
    ///
    /// **Does NOT pre-read source dimensions.** That's lazy via
    /// `source_dimensions(index)` from `display_preview_placeholder`,
    /// which only fires for the user's actual nav target. Pre-reading
    /// here for all 38 cached previews was a 7+ second main-thread block
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
            Preview {
                width,
                height,
                rgba,
            },
        );
        self.scheduler.mark_ready(index);
        // A fresh insert can push us over budget between navigations (previews
        // stream in over seconds), so enforce the byte budget on arrival too,
        // not only on `set_current`.
        self.evict_to_budget(self.current, preview_budget_bytes());
    }

    pub fn mark_failed(&mut self, index: usize, _request_id: RequestId) {
        // Worker handles its own entries cleanup via Forget message.
        self.scheduler.mark_failed(index);
    }

    /// Drop the cached preview for `path` (if this folder holds it) so a later request regenerates
    /// it from the now-modified file. Used by live folder sync on a `Modify` event: quicklookd
    /// keys its own on-disk cache on file content/mtime, so a fresh request after the edit yields
    /// fresh pixels — but our in-memory cache and the scheduler's `cached` set would otherwise pin
    /// the stale preview, so we evict both. Re-enqueues `path` for regeneration when it's inside
    /// the active generation window. No-op if `path` isn't in this folder.
    pub fn forget_path(&mut self, path: &Path) {
        let Some(index) = self.paths.iter().position(|p| p == path) else {
            return;
        };
        self.cache.remove(&index);
        // Let the scheduler re-queue it (it skips already-`cached` indices otherwise).
        self.scheduler.uncache(index);
        self.dim_prefetcher.invalidate(&[index]);
        // Re-warm the dim cache for the modified file (dimensions may have changed on a re-save).
        self.dim_prefetcher.enqueue(index, path.to_path_buf());
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

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1024 * 1024;
    const GB: usize = 1024 * MB;

    #[test]
    fn budget_scales_with_ram_and_clamps() {
        // The headline cases from the design: 1/128 of RAM, clamped.
        assert_eq!(budget_for_ram(64 * GB), 512 * MB);
        assert_eq!(budget_for_ram(16 * GB), 128 * MB);
        assert_eq!(budget_for_ram(8 * GB), 64 * MB); // floor (also exactly 8 GB / 128)
        assert_eq!(budget_for_ram(4 * GB), MIN_PREVIEW_BUDGET); // below floor → clamped up
        assert_eq!(budget_for_ram(256 * GB), MAX_PREVIEW_BUDGET); // above ceiling → clamped down
    }

    #[test]
    fn generation_radius_tracks_budget_and_is_capped() {
        // Big budget → full window; small budget → proportionally fewer previews,
        // so we never generate more than we'll retain.
        assert_eq!(
            generation_radius_for_budget(512 * MB),
            scheduler::WINDOW_RADIUS
        );
        assert_eq!(generation_radius_for_budget(128 * MB), 16);
        assert_eq!(generation_radius_for_budget(64 * MB), 8);
        // Never below the immediate-neighbor floor.
        assert!(generation_radius_for_budget(0) >= 2);
        // Never above the cap.
        assert!(generation_radius_for_budget(usize::MAX) <= scheduler::WINDOW_RADIUS);
    }

    fn insert_preview(state: &mut State, index: usize, bytes: usize) {
        state.cache.insert(
            index,
            Preview {
                width: 1,
                height: 1,
                rgba: vec![0u8; bytes],
            },
        );
    }

    #[test]
    fn evict_to_budget_keeps_nearest_to_current() {
        let mut state = State::new();
        // 10 previews of 10 MB each (100 MB total) at indices 0..=9.
        for i in 0..10 {
            insert_preview(&mut state, i, 10 * MB);
        }
        // Budget for 3 previews, current at index 5. Nearest are 4, 5, 6.
        state.evict_to_budget(5, 30 * MB);

        assert!(state.memory_bytes() <= 30 * MB, "must fit budget");
        assert_eq!(state.cache.len(), 3, "keeps exactly what fits");
        for keep in [4, 5, 6] {
            assert!(
                state.cache.contains_key(&keep),
                "should keep {keep} (near current)"
            );
        }
        for drop in [0, 1, 2, 3, 7, 8, 9] {
            assert!(
                !state.cache.contains_key(&drop),
                "should drop {drop} (far from current)"
            );
        }
    }

    #[test]
    fn evict_to_budget_noop_when_under() {
        let mut state = State::new();
        insert_preview(&mut state, 0, 10 * MB);
        insert_preview(&mut state, 1, 10 * MB);
        state.evict_to_budget(0, 512 * MB);
        assert_eq!(state.cache.len(), 2, "nothing evicted when under budget");
    }
}
