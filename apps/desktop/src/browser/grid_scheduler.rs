//! Grid thumbnail generation scheduler. Pure state machine — no OS calls, no I/O.
//!
//! Sibling of [`crate::previews::scheduler`], but prioritized around a **visible
//! range** (the rows currently on screen in the `NSCollectionView`) rather than a
//! single `current` index. The collection view feeds it the visible range on every
//! scroll; the scheduler hands back the order in which to ask QuickLook for grid
//! thumbnails so whatever is on-screen always generates first.
//!
//! ## Ordering
//!
//! Given a folder of N images and a visible `Range<usize>`, [`Scheduler::poll_next`]
//! emits indices in this order:
//!
//! 1. **The visible range itself**, left-to-right. These are the cells the user is
//!    looking at right now; their thumbnails must land first.
//! 2. **Outward from the visible range**, nearest-first (one below the range, one
//!    above, two below, two above, …), bounded by [`MARGIN`]. This warms a margin
//!    ahead/behind so a small scroll reveals already-generated cells — matching the
//!    `NSCollectionViewPrefetching` window the collection view wires up.
//!
//! Indices beyond `visible.end + MARGIN` / before `visible.start − MARGIN` are not
//! enqueued: a 10 000-image folder must not queue 10 000 jobs. On scroll the caller
//! calls [`Scheduler::set_visible_range`], which reseeds the queue centered on the
//! new range. Already-cached indices stay cached and are skipped.
//!
//! Distance-from-range (used both here and by [`super::thumbnail_cache`] for
//! eviction) is 0 inside the range, else the gap to the nearer edge.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;

/// How far beyond the visible range (in indices, each direction) we generate
/// thumbnails. Warms a margin so a small scroll reveals ready cells. Generation
/// past this waits for the next `set_visible_range`. Chosen to comfortably cover
/// one screen of prefetch ahead/behind for typical cell counts.
pub const MARGIN: usize = 100;

/// Opaque handle a caller can use to correlate a completion with its request.
/// Monotonic. Distinct type alias from `previews::scheduler::RequestId` so the two
/// schedulers can't accidentally cross-wire their ids.
pub type RequestId = u64;

/// Distance of `index` from a visible `range`: 0 if inside, else the gap to the
/// nearer edge. Shared with [`super::thumbnail_cache`] so scheduling and eviction
/// agree on "nearest the visible range".
#[must_use]
pub fn distance_from_range(index: usize, range: &Range<usize>) -> usize {
    if range.is_empty() {
        // Degenerate range: measure from its start.
        return index.abs_diff(range.start);
    }
    if index < range.start {
        range.start - index
    } else if index >= range.end {
        index - (range.end - 1)
    } else {
        0
    }
}

pub struct Scheduler {
    folder_len: usize,
    visible: Range<usize>,
    queue: VecDeque<usize>,
    in_flight: HashMap<usize, RequestId>,
    cached: HashSet<usize>,
    failed: HashSet<usize>,
    max_parallel: usize,
    paused: bool,
    next_request_id: RequestId,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub folder_len: usize,
    pub visible: Range<usize>,
    pub in_flight: Vec<usize>,
    pub queue_len: usize,
    pub cached: Vec<usize>,
    pub failed: Vec<usize>,
    pub paused: bool,
    pub max_parallel: usize,
}

impl Scheduler {
    #[must_use]
    pub fn new(max_parallel: usize) -> Self {
        Self {
            folder_len: 0,
            visible: 0..0,
            queue: VecDeque::new(),
            in_flight: HashMap::new(),
            cached: HashSet::new(),
            failed: HashSet::new(),
            max_parallel: max_parallel.max(1),
            paused: false,
            next_request_id: 1,
        }
    }

    /// Reset for a new folder. Clears cache/queue and reseeds centered on `visible`.
    /// In-flight requests are the caller's to cancel (they'll be dropped on arrival
    /// via generation/stale checks Phase 4 owns).
    pub fn set_folder(&mut self, folder_len: usize, visible: Range<usize>) {
        self.folder_len = folder_len;
        self.cached.clear();
        self.failed.clear();
        self.visible = clamp_range(visible, folder_len);
        self.rebuild_queue();
    }

    /// Update the visible range (called on scroll); reseeds the queue centered on it.
    /// Already-cached indices stay cached and don't re-enter the queue.
    pub fn set_visible_range(&mut self, visible: Range<usize>) {
        if self.folder_len == 0 {
            self.visible = 0..0;
            return;
        }
        self.visible = clamp_range(visible, self.folder_len);
        self.rebuild_queue();
    }

    /// The current visible range, clamped to the folder.
    #[must_use]
    pub fn visible_range(&self) -> Range<usize> {
        self.visible.clone()
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Mark a request completed successfully. The thumbnail is cached.
    pub fn mark_ready(&mut self, index: usize) {
        self.in_flight.remove(&index);
        self.cached.insert(index);
        self.failed.remove(&index);
    }

    /// Mark a request failed. Don't retry automatically.
    pub fn mark_failed(&mut self, index: usize) {
        self.in_flight.remove(&index);
        self.failed.insert(index);
    }

    /// Drop a previously-cached index back to "uncached" so it can be re-queued if
    /// it falls inside the active window again. Called when [`super::thumbnail_cache`]
    /// evicts — the scheduler's `cached` set must stay in sync or the index would be
    /// permanently skipped.
    pub fn uncache(&mut self, index: usize) {
        self.cached.remove(&index);
    }

    /// Pop the next index to request, if the parallelism cap and paused flag allow.
    /// Returns `(index, request_id)`. The caller fires the OS-level QL request.
    pub fn poll_next(&mut self) -> Option<(usize, RequestId)> {
        if self.paused {
            return None;
        }
        if self.in_flight.len() >= self.max_parallel {
            return None;
        }
        while let Some(index) = self.queue.pop_front() {
            if self.cached.contains(&index)
                || self.failed.contains(&index)
                || self.in_flight.contains_key(&index)
            {
                continue;
            }
            let id = self.next_request_id;
            self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
            self.in_flight.insert(index, id);
            return Some((index, id));
        }
        None
    }

    #[must_use]
    pub fn status(&self) -> Status {
        let mut in_flight: Vec<usize> = self.in_flight.keys().copied().collect();
        in_flight.sort_unstable();
        let mut cached: Vec<usize> = self.cached.iter().copied().collect();
        cached.sort_unstable();
        let mut failed: Vec<usize> = self.failed.iter().copied().collect();
        failed.sort_unstable();
        Status {
            folder_len: self.folder_len,
            visible: self.visible.clone(),
            in_flight,
            queue_len: self.queue.len(),
            cached,
            failed,
            paused: self.paused,
            max_parallel: self.max_parallel,
        }
    }

    /// Rebuild the queue from `folder_len` + `visible`. Phases:
    /// 1. The visible range, left-to-right (on-screen cells first).
    /// 2. Outward from the range, nearest-first, bounded by [`MARGIN`].
    fn rebuild_queue(&mut self) {
        self.queue.clear();
        if self.folder_len == 0 || self.visible.is_empty() {
            return;
        }
        let len = self.folder_len;
        // Phase 1: the visible cells, in reading order.
        for idx in self.visible.clone() {
            if idx < len {
                self.queue.push_back(idx);
            }
        }
        // Phase 2: margin, nearest-first, alternating below/above the range.
        let below_start = self.visible.end; // first index after the range
        let above_end = self.visible.start; // exclusive lower bound going up
        for step in 1..=MARGIN {
            // Below the range (indices ≥ visible.end), nearest first.
            let below = below_start + (step - 1);
            if below < len {
                self.queue.push_back(below);
            }
            // Above the range (indices < visible.start), nearest first.
            if step <= above_end {
                self.queue.push_back(above_end - step);
            }
        }
    }
}

/// Clamp a requested visible range to `[0, folder_len)`, returning an empty range
/// when the folder is empty. Keeps `start <= end <= folder_len`.
fn clamp_range(range: Range<usize>, folder_len: usize) -> Range<usize> {
    if folder_len == 0 {
        return 0..0;
    }
    let start = range.start.min(folder_len);
    let end = range.end.clamp(start, folder_len);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(s: &mut Scheduler) -> Vec<usize> {
        let mut order = Vec::new();
        while let Some((i, _)) = s.poll_next() {
            order.push(i);
            s.mark_ready(i);
        }
        order
    }

    #[test]
    fn empty_folder_no_work() {
        let mut s = Scheduler::new(5);
        s.set_folder(0, 0..10);
        assert!(s.poll_next().is_none());
        assert_eq!(s.status().folder_len, 0);
        assert!(s.visible_range().is_empty());
    }

    #[test]
    fn distance_from_range_is_zero_inside_else_edge_gap() {
        let r = 4..8; // indices 4,5,6,7 visible
        assert_eq!(distance_from_range(4, &r), 0);
        assert_eq!(distance_from_range(7, &r), 0);
        assert_eq!(distance_from_range(3, &r), 1); // one below start
        assert_eq!(distance_from_range(8, &r), 1); // one above last (7)
        assert_eq!(distance_from_range(0, &r), 4);
        assert_eq!(distance_from_range(10, &r), 3);
    }

    #[test]
    fn visible_range_first_then_nearest_outward() {
        // Folder of 12, visible 4..7 (cells 4,5,6). MARGIN large enough to reach all.
        let mut s = Scheduler::new(100);
        s.set_folder(12, 4..7);
        let order = drain(&mut s);
        // Phase 1: 4,5,6 (reading order). Phase 2: below first then above, each step:
        // step1 → 7 (below), 3 (above); step2 → 8, 2; step3 → 9, 1; step4 → 10, 0;
        // step5 → 11 (below; above exhausted).
        assert_eq!(order, vec![4, 5, 6, 7, 3, 8, 2, 9, 1, 10, 0, 11]);
    }

    #[test]
    fn margin_bounds_huge_folders() {
        // 10k folder, visible 5000..5010. Only the range + MARGIN each side may emit.
        let mut s = Scheduler::new(1);
        s.set_folder(10_000, 5000..5010);
        let order = drain(&mut s);
        // Upper bound: visible width (10) + 2*MARGIN.
        assert!(
            order.len() <= 10 + 2 * MARGIN,
            "queue should be bounded by margin, got {}",
            order.len()
        );
        // Every emitted index is within MARGIN of the range.
        for &i in &order {
            assert!(
                distance_from_range(i, &(5000..5010)) <= MARGIN,
                "index {i} is outside the margin"
            );
        }
        // Indices far outside must not appear.
        assert!(!order.contains(&100));
        assert!(!order.contains(&9000));
    }

    #[test]
    fn set_visible_range_reseeds_on_scroll() {
        let mut s = Scheduler::new(10);
        s.set_folder(10_000, 0..10);
        // Take a couple from the original window without marking done.
        let _ = s.poll_next();
        let _ = s.poll_next();
        assert_eq!(s.status().in_flight.len(), 2);
        // Scroll far away.
        s.set_visible_range(8000..8010);
        // In-flight survive the reseed; the next poll is near the new range.
        assert_eq!(s.status().in_flight.len(), 2);
        let next = s.poll_next().map(|(i, _)| i).unwrap();
        assert!(
            distance_from_range(next, &(8000..8010)) <= MARGIN,
            "after scroll to 8000, next emit was {next}"
        );
    }

    #[test]
    fn cached_indices_skipped() {
        let mut s = Scheduler::new(10);
        s.set_folder(12, 4..7);
        s.mark_ready(5);
        s.mark_ready(8);
        let order = {
            let mut order = Vec::new();
            while let Some((i, _)) = s.poll_next() {
                order.push(i);
                s.mark_ready(i);
            }
            order
        };
        // 5 and 8 already cached → skipped; rest in priority order.
        assert!(!order.contains(&5));
        assert!(!order.contains(&8));
        assert_eq!(order.first(), Some(&4)); // visible range still leads
    }

    #[test]
    fn parallelism_cap_gates_poll() {
        let mut s = Scheduler::new(2);
        s.set_folder(20, 0..5);
        let (first, _) = s.poll_next().unwrap();
        assert!(s.poll_next().is_some());
        assert!(s.poll_next().is_none(), "third poll gated by max_parallel");
        s.mark_ready(first);
        assert!(s.poll_next().is_some());
    }

    #[test]
    fn paused_blocks_poll() {
        let mut s = Scheduler::new(5);
        s.set_folder(20, 0..5);
        s.pause();
        assert!(s.poll_next().is_none());
        s.resume();
        assert!(s.poll_next().is_some());
    }

    #[test]
    fn uncache_lets_index_requeue() {
        let mut s = Scheduler::new(10);
        s.set_folder(12, 4..7);
        s.mark_ready(4);
        // Re-seed; 4 stays cached so it's skipped.
        s.set_visible_range(4..7);
        let order_before = drain(&mut s);
        assert!(!order_before.contains(&4));
        // Evict 4: scheduler must re-queue it on the next reseed.
        s.uncache(4);
        s.set_visible_range(4..7);
        let next = s.poll_next().map(|(i, _)| i);
        assert_eq!(
            next,
            Some(4),
            "uncached visible index should re-queue first"
        );
    }

    #[test]
    fn range_clamped_to_folder() {
        let mut s = Scheduler::new(10);
        // Visible range overshoots the folder; clamp to [0, 6).
        s.set_folder(6, 3..100);
        assert_eq!(s.visible_range(), 3..6);
        let order = drain(&mut s);
        // Visible 3,4,5 first, then below exhausted, above: 2,1,0.
        assert_eq!(order, vec![3, 4, 5, 2, 1, 0]);
    }
}
