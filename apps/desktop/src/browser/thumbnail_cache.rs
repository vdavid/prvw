//! Grid thumbnail cache bookkeeping — a pure, headless-tested state machine.
//!
//! Phase 2 builds only the **policy**: a byte-budgeted, distance-from-visible-range
//! eviction map keyed by folder index. The real bitmaps (`NSImage`) live AppKit-side,
//! owned by the `NSCollectionView` cells and released on cell reuse (Phase 4) — this
//! map never holds pixels, only each entry's **byte size**. So this is the LRU
//! backstop that bounds the small-cell worst case (many tiny cells visible at once),
//! not the primary residency mechanism; cell reuse does most of the work.
//!
//! Eviction mirrors [`crate::previews`]' distance policy but centers on a visible
//! **range** (the rows on screen) rather than a single `current` index, sharing
//! [`super::grid_scheduler::distance_from_range`] so scheduling and eviction agree on
//! "nearest the visible range". On `insert`, `get` (touch), and an explicit
//! [`ThumbnailCache::evict_to_budget`], entries farthest from the range are dropped
//! first until the total fits the budget.

use super::grid_scheduler::distance_from_range;
use std::collections::HashMap;
use std::ops::Range;

/// Physical px of a grid thumbnail at the slider's max cell size.
///
/// Grid thumbnails are generated **once** at `MAX_CELL_PT × 2` physical px (Retina)
/// and downscaled for smaller display sizes — never regenerated on resize. At the
/// max cell of 256pt that's 512px. One RGBA8 bitmap is `512 × 512 × 4 ≈ 1 MB`, so the
/// 128 MB budget below holds ~128 resident thumbnails — the small-cell worst case
/// (many tiny cells visible) bounded. Phase 4's image views downscale this cached
/// bitmap; the slider never re-requests. Reused by Phase 4 for the QL request size.
pub const MAX_CELL_PT: u32 = 256;

/// Physical px a grid thumbnail is generated at: `MAX_CELL_PT × 2` (Retina).
pub const GRID_THUMBNAIL_PX: u32 = MAX_CELL_PT * 2; // 512

/// Estimated bytes of one max-size grid thumbnail: `512 × 512 × 4 (RGBA8) ≈ 1 MB`.
/// Used only for the budget's headline "~128 thumbnails" framing; real accounting
/// uses each entry's reported byte size.
pub const EST_THUMBNAIL_BYTES: usize =
    (GRID_THUMBNAIL_PX as usize) * (GRID_THUMBNAIL_PX as usize) * 4;

/// LRU backstop for resident grid thumbnails: 128 MB ≈ 128 max-size thumbnails.
/// `NSCollectionView` cell reuse already releases off-screen images, so resident
/// memory tracks the visible set, not the folder size — a 10k-image folder costs the
/// same as a 50-image one. This budget bounds the small-cell worst case where many
/// tiny cells are visible at once. Fixed (not RAM-scaled like previews) because cell
/// reuse, not this map, is the primary bound.
pub const THUMBNAIL_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// One cached thumbnail's bookkeeping. No pixels — AppKit owns those (Phase 4). We
/// store the byte size for budgeting and a monotonic `touched` tick for LRU tie-breaks
/// among equidistant entries.
#[derive(Debug, Clone, Copy)]
struct Entry {
    bytes: usize,
    touched: u64,
}

/// Byte-budgeted, distance-from-visible-range thumbnail cache **state** (no bitmaps).
pub struct ThumbnailCache {
    entries: HashMap<usize, Entry>,
    visible: Range<usize>,
    budget: usize,
    total_bytes: usize,
    tick: u64,
}

impl ThumbnailCache {
    /// New cache with the default [`THUMBNAIL_BUDGET_BYTES`] budget.
    #[must_use]
    pub fn new() -> Self {
        Self::with_budget(THUMBNAIL_BUDGET_BYTES)
    }

    /// New cache with an explicit byte budget. Lets tests exercise eviction without
    /// allocating 128 MB worth of entries.
    #[must_use]
    pub fn with_budget(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            visible: 0..0,
            budget,
            total_bytes: 0,
            tick: 0,
        }
    }

    /// Update the visible range, then evict to budget around it. Called on scroll and
    /// folder change so the cache always keeps the entries nearest where the user is.
    pub fn set_visible_range(&mut self, visible: Range<usize>) {
        self.visible = visible;
        self.evict_to_budget();
    }

    /// The current visible range.
    #[must_use]
    pub fn visible_range(&self) -> Range<usize> {
        self.visible.clone()
    }

    /// Total resident bytes across all cached thumbnails.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Number of resident thumbnails.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// True if a thumbnail for `index` is resident.
    #[must_use]
    pub fn contains(&self, index: usize) -> bool {
        self.entries.contains_key(&index)
    }

    /// Insert (or replace) the thumbnail bookkeeping for `index` with `bytes`, then
    /// evict to budget. Replacing updates the byte total by the delta.
    pub fn insert(&mut self, index: usize, bytes: usize) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(prev) = self.entries.insert(
            index,
            Entry {
                bytes,
                touched: tick,
            },
        ) {
            self.total_bytes = self.total_bytes - prev.bytes + bytes;
        } else {
            self.total_bytes += bytes;
        }
        self.evict_to_budget();
    }

    /// Touch `index` as most-recently-used and report whether it's resident. Phase 4
    /// calls this when a cell rebinds to a cached thumbnail so the LRU tick reflects
    /// real use (tie-break among equidistant entries). Returns `true` on a hit.
    pub fn get(&mut self, index: usize) -> bool {
        self.tick += 1;
        let tick = self.tick;
        if let Some(entry) = self.entries.get_mut(&index) {
            entry.touched = tick;
            true
        } else {
            false
        }
    }

    /// Drop `index` if resident, returning its byte size. Phase 4 calls this when a
    /// cell is reused for a different index and AppKit releases the bitmap.
    pub fn remove(&mut self, index: usize) -> Option<usize> {
        let entry = self.entries.remove(&index)?;
        self.total_bytes -= entry.bytes;
        Some(entry.bytes)
    }

    /// Evict entries farthest from the visible range first, breaking ties by least-
    /// recently-touched, until the total fits the budget. Returns the evicted indices
    /// so the caller can drop them from [`super::grid_scheduler`]'s `cached` set
    /// (`uncache`) — otherwise an evicted index would be permanently skipped.
    pub fn evict_to_budget(&mut self) -> Vec<usize> {
        if self.total_bytes <= self.budget {
            return Vec::new();
        }
        // Order candidates: farthest from the range first, then oldest touch first.
        let visible = self.visible.clone();
        let mut candidates: Vec<(usize, usize, u64)> = self
            .entries
            .iter()
            .map(|(&idx, e)| (idx, distance_from_range(idx, &visible), e.touched))
            .collect();
        candidates.sort_by(|a, b| {
            // Higher distance evicted first; then lower touch (older) first.
            b.1.cmp(&a.1).then(a.2.cmp(&b.2))
        });

        let mut evicted = Vec::new();
        for (idx, _dist, _touched) in candidates {
            if self.total_bytes <= self.budget {
                break;
            }
            if let Some(entry) = self.entries.remove(&idx) {
                self.total_bytes -= entry.bytes;
                evicted.push(idx);
            }
        }
        evicted
    }
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1024 * 1024;

    #[test]
    fn size_constant_matches_memory_math() {
        // Max cell 256pt → 512px physical → ~1 MB per RGBA8 thumbnail.
        assert_eq!(GRID_THUMBNAIL_PX, 512);
        assert_eq!(EST_THUMBNAIL_BYTES, 512 * 512 * 4);
        assert_eq!(EST_THUMBNAIL_BYTES, MB); // exactly 1 MiB
        // 128 MB budget ≈ 128 max-size thumbnails.
        assert_eq!(THUMBNAIL_BUDGET_BYTES / EST_THUMBNAIL_BYTES, 128);
    }

    #[test]
    fn insert_get_remove_track_bytes() {
        let mut c = ThumbnailCache::with_budget(100 * MB);
        assert!(c.is_empty());
        c.insert(0, MB);
        c.insert(1, 2 * MB);
        assert_eq!(c.len(), 2);
        assert_eq!(c.total_bytes(), 3 * MB);
        assert!(c.get(0));
        assert!(!c.get(99));
        assert_eq!(c.remove(1), Some(2 * MB));
        assert_eq!(c.total_bytes(), MB);
        assert_eq!(c.remove(1), None);
    }

    #[test]
    fn replacing_an_entry_adjusts_total_by_delta() {
        let mut c = ThumbnailCache::with_budget(100 * MB);
        c.insert(0, MB);
        c.insert(0, 4 * MB); // replace
        assert_eq!(c.len(), 1);
        assert_eq!(c.total_bytes(), 4 * MB);
    }

    #[test]
    fn eviction_keeps_entries_nearest_the_visible_range() {
        // 12 thumbnails of 10 MB at indices 0..=11; budget for 3 (30 MB).
        // Visible range 4..7 (cells 4,5,6) — keep the three nearest it. The range is
        // set first (real flow: scroll sets the range, then thumbnails arrive), so the
        // inline eviction on each insert already centers on it.
        let mut c = ThumbnailCache::with_budget(30 * MB);
        c.set_visible_range(4..7);
        for i in 0..12 {
            c.insert(i, 10 * MB);
        }
        c.evict_to_budget();
        assert!(c.total_bytes() <= 30 * MB, "must fit budget");
        // The three nearest (distance 0) are exactly the visible cells.
        assert_eq!(c.len(), 3);
        for keep in [4, 5, 6] {
            assert!(c.contains(keep), "should keep {keep} (in visible range)");
        }
        for drop in [0, 1, 2, 3, 7, 8, 9, 10, 11] {
            assert!(!c.contains(drop), "should drop {drop} (outside range)");
        }
    }

    #[test]
    fn eviction_returns_evicted_for_scheduler_resync() {
        let mut c = ThumbnailCache::with_budget(20 * MB);
        for i in 0..5 {
            c.insert(i, 10 * MB);
        }
        c.set_visible_range(0..1); // keep nearest index 0
        let evicted = c.evict_to_budget();
        // Already at/under budget after set_visible_range evicted; this call is a no-op.
        assert!(evicted.is_empty());
        assert!(c.total_bytes() <= 20 * MB);
        // The far entries were evicted by set_visible_range.
        assert!(c.contains(0));
        assert!(c.contains(1));
        assert!(!c.contains(4));
    }

    #[test]
    fn insert_evicts_inline_when_over_budget() {
        let mut c = ThumbnailCache::with_budget(20 * MB);
        c.set_visible_range(0..1);
        c.insert(0, 10 * MB);
        c.insert(1, 10 * MB);
        c.insert(5, 10 * MB); // would be 30 MB; 5 is farthest from range 0..1
        assert!(c.total_bytes() <= 20 * MB);
        assert!(!c.contains(5), "farthest from range evicted on insert");
        assert!(c.contains(0));
    }

    #[test]
    fn ties_broken_by_least_recently_touched() {
        // Two equidistant entries, budget for one. The less-recently-touched is evicted.
        let mut c = ThumbnailCache::with_budget(10 * MB);
        c.set_visible_range(6..7); // cell 6; both 5 and 7 are distance 1
        c.insert(5, 10 * MB);
        c.insert(7, 10 * MB);
        c.get(7); // touch 7 → 5 is now older
        c.evict_to_budget();
        assert_eq!(c.len(), 1);
        assert!(c.contains(7), "recently-touched survives the tie");
        assert!(!c.contains(5));
    }

    #[test]
    fn noop_when_under_budget() {
        let mut c = ThumbnailCache::with_budget(512 * MB);
        c.insert(0, 10 * MB);
        c.insert(1, 10 * MB);
        let evicted = c.evict_to_budget();
        assert!(evicted.is_empty());
        assert_eq!(c.len(), 2);
    }
}
