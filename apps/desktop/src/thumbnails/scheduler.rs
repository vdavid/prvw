//! Thumbnail generation scheduler. Pure state machine — no OS calls, no I/O.
//!
//! Given a folder of N images and a current index, emits requests in the
//! order that best serves the user:
//!
//! 1. **Immediate neighbors** (`dist 1..=PRELOAD_HALF`), centered outward.
//!    The user's most likely next nav target is one step away; a thumb
//!    placeholder for that should be ready *first* even though the
//!    full-decode preloader is also working on it (the placeholder shows
//!    while the primary decode is still in flight, sometimes for 500 ms+
//!    on a slow share).
//! 2. **Outside the preload window** (`dist > PRELOAD_HALF`), centered-
//!    outward, **bounded by [`WINDOW_RADIUS`]**. These cover the
//!    "exploration" navigations.
//! 3. `current` itself, last. Primary decode is almost always faster than
//!    a thumb fetch for an index we're actively loading, and we don't
//!    need a placeholder for the image we're displaying anyway.
//!
//! Indices outside the window aren't enqueued at all. For a 10 000-image
//! folder we'd otherwise queue 10 000 thumbnail jobs at startup —
//! quicklookd serves ~7/sec, so it'd take 24 minutes to drain, with most
//! of the work going to indices the user will never visit. Windowed
//! scheduling caps the work at `2 × WINDOW_RADIUS` thumbs (~14 sec at the
//! current radius) and reseeds when the user navigates.
//!
//! Caps in-flight requests at `max_parallel` to avoid stressing the system.
//! Supports pause/resume so a primary decode can get first dibs on I/O and
//! shared CPU.

use std::collections::{HashMap, HashSet, VecDeque};

/// Distance from `current` within which full-decode preloads cover the index
/// (matches `navigation::preloader::PRELOAD_AHEAD`).
const PRELOAD_HALF: usize = 2;

/// Upper bound on how far around `current` we generate thumbnails. The
/// effective radius is set per-`Scheduler` via [`Scheduler::with_window_radius`]
/// and never exceeds this — `thumbnails::generation_radius` derives it from the
/// RAM-scaled cache budget so we never generate more than we'll retain. ~50
/// means ~100 thumbs, ~14 sec to populate at quicklookd's ~7/sec serving rate.
pub const WINDOW_RADIUS: usize = 50;

/// Opaque handle a caller can use to cancel a specific request. Monotonic.
pub type RequestId = u64;

pub struct Scheduler {
    folder_len: usize,
    current: usize,
    queue: VecDeque<usize>,
    in_flight: HashMap<usize, RequestId>,
    cached: HashSet<usize>,
    failed: HashSet<usize>,
    max_parallel: usize,
    /// Effective generation radius, ≤ [`WINDOW_RADIUS`]. Set via
    /// [`Scheduler::with_window_radius`]; defaults to the cap.
    window_radius: usize,
    paused: bool,
    next_request_id: RequestId,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub folder_len: usize,
    pub current: usize,
    pub in_flight: Vec<usize>,
    pub queue_len: usize,
    pub cached: Vec<usize>,
    pub failed: Vec<usize>,
    pub paused: bool,
    pub max_parallel: usize,
}

impl Scheduler {
    pub fn new(max_parallel: usize) -> Self {
        Self {
            folder_len: 0,
            current: 0,
            queue: VecDeque::new(),
            in_flight: HashMap::new(),
            cached: HashSet::new(),
            failed: HashSet::new(),
            max_parallel: max_parallel.max(1),
            window_radius: WINDOW_RADIUS,
            paused: false,
            next_request_id: 1,
        }
    }

    /// Set the generation radius (clamped to `[PRELOAD_HALF, WINDOW_RADIUS]`).
    /// `thumbnails::State` derives this from the RAM-scaled cache budget so we
    /// never enqueue thumbnails the byte-budgeted cache would immediately evict.
    pub fn with_window_radius(mut self, radius: usize) -> Self {
        self.window_radius = radius.clamp(PRELOAD_HALF, WINDOW_RADIUS);
        self
    }

    /// The effective generation radius. Used by `thumbnails::State` to size the
    /// dimension-prefetch window to match.
    pub fn window_radius(&self) -> usize {
        self.window_radius
    }

    /// Reset for a new folder. Clears cache/queue; caller cancels in-flight
    /// requests separately by calling `drain_in_flight`.
    pub fn set_folder(&mut self, folder_len: usize, current: usize) {
        self.folder_len = folder_len;
        self.current = current.min(folder_len.saturating_sub(1));
        self.cached.clear();
        self.failed.clear();
        self.rebuild_queue();
    }

    /// Update the current index; rebuilds the queue in the new centered order.
    /// Already-cached indices stay cached and don't re-enter the queue.
    pub fn set_current(&mut self, current: usize) {
        if self.folder_len == 0 {
            return;
        }
        self.current = current.min(self.folder_len - 1);
        self.rebuild_queue();
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Mark a request completed successfully. The rendered thumb is cached.
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

    /// Drop a previously-cached index back to "uncached" so it can be
    /// re-queued if it ever falls inside the active window again.
    /// Called by `State::evict_distant_thumbs` when the RAM cache evicts
    /// — the scheduler's `cached` set must stay in sync or the index
    /// would get permanently skipped.
    pub fn uncache(&mut self, index: usize) {
        self.cached.remove(&index);
    }

    /// Pop the next index to request, if allowed by the parallelism cap
    /// and the paused flag. Returns `(index, request_id)`. Caller is
    /// responsible for actually firing the OS-level request.
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

    pub fn status(&self) -> Status {
        let mut in_flight: Vec<usize> = self.in_flight.keys().copied().collect();
        in_flight.sort_unstable();
        let mut cached: Vec<usize> = self.cached.iter().copied().collect();
        cached.sort_unstable();
        let mut failed: Vec<usize> = self.failed.iter().copied().collect();
        failed.sort_unstable();
        Status {
            folder_len: self.folder_len,
            current: self.current,
            in_flight,
            queue_len: self.queue.len(),
            cached,
            failed,
            paused: self.paused,
            max_parallel: self.max_parallel,
        }
    }

    /// Rebuild the queue from scratch based on current `folder_len` and
    /// `current`. Phases:
    /// 1. Immediate neighbors (dist 1..=PRELOAD_HALF), centered outward —
    ///    most likely next nav target, must have a placeholder ready
    ///    *before* the user presses arrow-left/right.
    /// 2. Outside preload window (dist > PRELOAD_HALF), capped by
    ///    [`WINDOW_RADIUS`].
    /// 3. `current` itself last.
    ///
    /// Bounded by [`WINDOW_RADIUS`] so 10k-image folders don't queue 10k
    /// jobs.
    fn rebuild_queue(&mut self) {
        self.queue.clear();
        if self.folder_len == 0 {
            return;
        }
        let cur = self.current as isize;
        let len = self.folder_len as isize;
        let half = PRELOAD_HALF as isize;
        // Cap the outer distance at the smaller of the folder bound and the
        // effective window radius. For folders smaller than the radius this
        // collapses to "all indices" — same shape as before windowing.
        let max_dist = (len - 1).min(self.window_radius as isize).max(0);

        let push = |queue: &mut VecDeque<usize>, idx: isize| {
            if idx >= 0 && idx < len {
                queue.push_back(idx as usize);
            }
        };

        // Phase 1: immediate neighbors first, centered outward.
        for dist in 1..=half.min(max_dist) {
            push(&mut self.queue, cur + dist);
            push(&mut self.queue, cur - dist);
        }
        // Phase 2: distances > PRELOAD_HALF, centered outward.
        for dist in (half + 1)..=max_dist {
            push(&mut self.queue, cur + dist);
            push(&mut self.queue, cur - dist);
        }
        // Phase 3: current itself, last.
        push(&mut self.queue, cur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_folder_no_work() {
        let mut s = Scheduler::new(5);
        s.set_folder(0, 0);
        assert!(s.poll_next().is_none());
        assert_eq!(s.status().folder_len, 0);
    }

    #[test]
    fn small_folder_immediate_neighbors_first() {
        // folder of 6, current = 0. PRELOAD_HALF = 2. Phase 1 (dist 1..=2):
        // 1, 2. Phase 2 (dist > 2, outside preload window): 3, 4, 5.
        // Phase 3: 0 (current, last).
        let mut s = Scheduler::new(10);
        s.set_folder(6, 0);
        let mut order = Vec::new();
        while let Some((i, _)) = s.poll_next() {
            order.push(i);
        }
        assert_eq!(order, vec![1, 2, 3, 4, 5, 0]);
    }

    #[test]
    fn centered_traversal_mid_folder() {
        // folder of 11, current = 5. PRELOAD_HALF = 2.
        // Phase 1 (immediate neighbors, dist 1..=2 outward): 6, 4, 7, 3.
        // Phase 2 (dist > 2, outward): 8, 2, 9, 1, 10, 0.
        // Phase 3: 5.
        let mut s = Scheduler::new(100);
        s.set_folder(11, 5);
        let mut order = Vec::new();
        while let Some((i, _)) = s.poll_next() {
            order.push(i);
        }
        assert_eq!(order, vec![6, 4, 7, 3, 8, 2, 9, 1, 10, 0, 5]);
    }

    #[test]
    fn parallelism_cap_gates_poll() {
        let mut s = Scheduler::new(2);
        s.set_folder(10, 0);
        // Poll twice — get two.
        let (first, _) = s.poll_next().unwrap();
        assert!(s.poll_next().is_some());
        // Third poll is gated by max_parallel until we mark one done.
        assert!(s.poll_next().is_none());
        s.mark_ready(first);
        assert!(s.poll_next().is_some());
    }

    #[test]
    fn paused_blocks_poll() {
        let mut s = Scheduler::new(5);
        s.set_folder(10, 0);
        s.pause();
        assert!(s.poll_next().is_none());
        s.resume();
        assert!(s.poll_next().is_some());
    }

    #[test]
    fn set_current_reseeds_queue() {
        let mut s = Scheduler::new(10);
        s.set_folder(11, 0);
        // Drain a couple without marking done (they go to in-flight).
        let _ = s.poll_next();
        let _ = s.poll_next();
        // Reseeding with a different current should clear the queue and
        // rebuild — but in-flight stay in-flight.
        s.set_current(10);
        assert_eq!(s.status().in_flight.len(), 2);
        // After rebuild centered on 10, Phase 1 (dist 1..=2): 11 (oob),
        // 9, 12 (oob), 8 → effectively 9, 8 (forward 11/12 out of range).
        // First pop = 9.
        let first = s.poll_next().map(|(i, _)| i);
        assert_eq!(first, Some(9));
    }

    #[test]
    fn cached_indices_skipped() {
        let mut s = Scheduler::new(10);
        s.set_folder(5, 2);
        s.mark_ready(0);
        s.mark_ready(4);
        let mut order = Vec::new();
        while let Some((i, _)) = s.poll_next() {
            order.push(i);
        }
        // Cached 0 and 4 are skipped; remaining in priority order from 2.
        // Phase 1 (dist 1..=2, outward from 2): 3, 1, 4, 0. But 4 and 0
        // cached, so: 3, 1. Phase 2 (dist > 2): none (max dist is 2).
        // Phase 3: 2.
        assert_eq!(order, vec![3, 1, 2]);
    }

    #[test]
    fn single_image_folder() {
        let mut s = Scheduler::new(5);
        s.set_folder(1, 0);
        // The only index is current. It goes last — so one poll returns it.
        let first = s.poll_next();
        assert_eq!(first.map(|(i, _)| i), Some(0));
        assert!(s.poll_next().is_none());
    }

    #[test]
    fn windowing_caps_huge_folders() {
        // 10k folder, current = 5000. Without windowing we'd queue 10k.
        // With WINDOW_RADIUS = 50 we should get at most ~101 indices
        // (50 forward + 50 backward + current).
        let mut s = Scheduler::new(1);
        s.set_folder(10_000, 5000);
        let mut order = Vec::new();
        // Drain everything (with parallelism=1 we get them one by one).
        while let Some((i, _)) = s.poll_next() {
            order.push(i);
            // Mark done so the next poll returns the next index.
            s.mark_ready(i);
        }
        let radius = WINDOW_RADIUS;
        assert!(
            order.len() <= 2 * radius + 1,
            "queue should be bounded by window radius, got {} indices",
            order.len()
        );
        // Every emitted index must be within the window.
        for i in &order {
            let dist = (*i as isize - 5000).unsigned_abs();
            assert!(
                dist <= radius,
                "index {i} is outside window of radius {radius}"
            );
        }
        // Indices well outside the window must NOT have been emitted.
        assert!(!order.contains(&100));
        assert!(!order.contains(&9000));
    }

    #[test]
    fn windowing_reseeds_on_set_current() {
        // After a far jump, the new window centers on the new current.
        let mut s = Scheduler::new(1);
        s.set_folder(10_000, 100);
        // Drain a few from the original window.
        for _ in 0..5 {
            if let Some((i, _)) = s.poll_next() {
                s.mark_ready(i);
            }
        }
        // Jump far away.
        s.set_current(8000);
        // Next index should be near 8000, not near 100.
        let next = s.poll_next().map(|(i, _)| i).unwrap();
        let dist_from_new = (next as isize - 8000).unsigned_abs();
        assert!(
            dist_from_new <= WINDOW_RADIUS,
            "after jump to 8000, next emit was {next} (dist {dist_from_new})"
        );
    }
}
