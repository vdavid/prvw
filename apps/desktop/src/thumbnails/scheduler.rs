//! Thumbnail generation scheduler. Pure state machine — no OS calls, no I/O.
//!
//! Given a folder of N images and a current index, emits requests in the
//! order that best serves the user:
//!
//! 1. Indices outside the preload window (`|i − current| > 2`), centered-
//!    outward. These are high-value because the full-decode preloader
//!    won't cover them.
//! 2. Indices inside the preload window, centered-outward. Lower value —
//!    the full-decode preloader will hit them soon anyway.
//! 3. `current` itself, last. Primary decode is almost always faster than
//!    a thumb fetch for an index we're actively loading.
//!
//! Caps in-flight requests at `max_parallel` to avoid stressing the system.
//! Supports pause/resume so a primary decode can get first dibs on I/O and
//! shared CPU.

use std::collections::{HashMap, HashSet, VecDeque};

/// Distance from `current` within which full-decode preloads cover the index
/// (matches `navigation::preloader::PRELOAD_AHEAD`).
const PRELOAD_HALF: usize = 2;

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
            paused: false,
            next_request_id: 1,
        }
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
    /// `current`. High-priority phase first (outside preload window), then
    /// low-priority (inside preload window), `current` last.
    fn rebuild_queue(&mut self) {
        self.queue.clear();
        if self.folder_len == 0 {
            return;
        }
        let cur = self.current as isize;
        let len = self.folder_len as isize;
        let half = PRELOAD_HALF as isize;

        let push = |queue: &mut VecDeque<usize>, idx: isize| {
            if idx >= 0 && idx < len {
                queue.push_back(idx as usize);
            }
        };

        // Phase 1: distances > PRELOAD_HALF, centered outward.
        for dist in (half + 1)..=len {
            push(&mut self.queue, cur + dist);
            push(&mut self.queue, cur - dist);
        }
        // Phase 2: distances 1..=PRELOAD_HALF, centered outward.
        for dist in 1..=half {
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
    fn small_folder_hits_preload_window_last() {
        // folder of 6, current = 0. Indices 1, 2 are inside the preload
        // window (|i - 0| <= 2). Indices 3, 4, 5 are outside.
        let mut s = Scheduler::new(10);
        s.set_folder(6, 0);
        let mut order = Vec::new();
        while let Some((i, _)) = s.poll_next() {
            order.push(i);
        }
        // Expected: 3, 4, 5 first (outside window, outward), then 1, 2
        // (inside window), then 0 (current, last).
        assert_eq!(order, vec![3, 4, 5, 1, 2, 0]);
    }

    #[test]
    fn centered_traversal_mid_folder() {
        // folder of 11, current = 5. Window is [3..=7]. Outside: 8, 2, 9, 1, 10, 0.
        // Inside outward: 6, 4, 7, 3. Current: 5.
        let mut s = Scheduler::new(100);
        s.set_folder(11, 5);
        let mut order = Vec::new();
        while let Some((i, _)) = s.poll_next() {
            order.push(i);
        }
        assert_eq!(order, vec![8, 2, 9, 1, 10, 0, 6, 4, 7, 3, 5]);
    }

    #[test]
    fn parallelism_cap_gates_poll() {
        let mut s = Scheduler::new(2);
        s.set_folder(10, 0);
        // Poll twice — get two.
        assert!(s.poll_next().is_some());
        assert!(s.poll_next().is_some());
        // Third poll is gated by max_parallel until we mark one done.
        assert!(s.poll_next().is_none());
        s.mark_ready(3);
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
        // After rebuild, the queue's first index should be from near
        // current=10 (outside window phase starts at dist 3 from 10: so 7).
        // Distances > 2 from 10: 7, 6, 5, 4, 3, 2, 1, 0.
        // forward from 10: none (out of bounds). Backward: 7, 6, 5, 4, 3, 2, 1, 0.
        // First pop (skipping any already-in-flight or cached) = 7.
        let first = s.poll_next().map(|(i, _)| i);
        assert_eq!(first, Some(7));
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
        // Cached 0 and 4 are skipped; remaining in centered order from 2.
        // Outside window (dist > 2 from 2): none (max dist is 2).
        // Inside window: 3, 1, 4, 0. But 4 and 0 cached, so: 3, 1.
        // Then current: 2.
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
}
