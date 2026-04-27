//! Loop-aware navigation helpers.
//!
//! `active_preload_indices` builds the wrap-aware preload window around the
//! current image. `step_next` / `step_previous` compute the next index when
//! the user navigates one step, returning `None` when navigation halts at a
//! directory edge (loop off) or when the directory is empty.
//!
//! Pure functions. No I/O, no decoding, no preloader interaction. Cache and
//! preloader callers compute the new window with these helpers, then act on
//! the difference.

/// Indices to keep warm around `current` (inclusive). With loop on, the
/// window wraps at the directory boundary so the user feels no edge. With
/// loop off, the window stops at `0` and `total - 1`. The result always
/// contains `current` first; remaining indices follow a forward-then-back
/// alternation, deduped, capped at `total` entries.
pub fn active_preload_indices(
    current: usize,
    total: usize,
    radius: usize,
    loop_on: bool,
) -> Vec<usize> {
    if total == 0 {
        return Vec::new();
    }
    if total == 1 {
        return vec![0];
    }
    let mut out: Vec<usize> = Vec::with_capacity(2 * radius + 1);
    out.push(current);
    let mut seen = vec![false; total];
    seen[current] = true;
    for step in 1..=radius {
        if let Some(idx) = step_forward(current, total, step, loop_on)
            && !seen[idx]
        {
            seen[idx] = true;
            out.push(idx);
        }
        if let Some(idx) = step_backward(current, total, step, loop_on)
            && !seen[idx]
        {
            seen[idx] = true;
            out.push(idx);
        }
        if out.len() == total {
            break;
        }
    }
    out
}

/// Compute the index after navigating one step forward. Returns `None`
/// when the directory is empty, or when `loop_on` is false and `current`
/// is already the last index.
///
/// Today the live path goes through `DirectoryList::go_by(delta, loop_on)`
/// because navigation is delta-based (the debounced path coalesces ±1
/// presses into one jump). This helper exists as the canonical single-step
/// formula tests can pin down without standing up a full `DirectoryList`.
#[allow(dead_code)]
pub fn step_next(current: usize, total: usize, loop_on: bool) -> Option<usize> {
    if total == 0 {
        return None;
    }
    if current + 1 < total {
        Some(current + 1)
    } else if loop_on {
        Some(0)
    } else {
        None
    }
}

/// Compute the index after navigating one step backward. Returns `None`
/// when the directory is empty, or when `loop_on` is false and `current`
/// is already the first index.
///
/// See `step_next` for why the live path doesn't call this directly.
#[allow(dead_code)]
pub fn step_previous(current: usize, total: usize, loop_on: bool) -> Option<usize> {
    if total == 0 {
        return None;
    }
    if current > 0 {
        Some(current - 1)
    } else if loop_on {
        Some(total - 1)
    } else {
        None
    }
}

fn step_forward(current: usize, total: usize, step: usize, loop_on: bool) -> Option<usize> {
    let raw = current.checked_add(step)?;
    if raw < total {
        Some(raw)
    } else if loop_on {
        Some(raw % total)
    } else {
        None
    }
}

fn step_backward(current: usize, total: usize, step: usize, loop_on: bool) -> Option<usize> {
    if current >= step {
        Some(current - step)
    } else if loop_on {
        // (current - step) modulo total, handled in signed arithmetic so we
        // don't underflow. `step <= total` is guaranteed by the caller (radius
        // capped via the dedup loop), but stay defensive with the modulo.
        let total_i = total as isize;
        let raw = current as isize - step as isize;
        let wrapped = ((raw % total_i) + total_i) % total_i;
        Some(wrapped as usize)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn preload_window_wraps_at_end_when_loop_on() {
        // 10 images, current = 9 (last), radius = 2, loop on.
        // Window must contain current + 2 ahead (wrap to 0, 1) + 2 behind (8, 7).
        let window = active_preload_indices(9, 10, 2, true);
        let set: HashSet<usize> = window.iter().copied().collect();
        assert_eq!(set, HashSet::from([9, 0, 1, 8, 7]));
        assert_eq!(window[0], 9, "current index is first");
    }

    #[test]
    fn preload_window_wraps_at_start_when_loop_on() {
        // 10 images, current = 0, radius = 2, loop on.
        // Behind wraps to last, last-1.
        let window = active_preload_indices(0, 10, 2, true);
        let set: HashSet<usize> = window.iter().copied().collect();
        assert_eq!(set, HashSet::from([0, 1, 2, 9, 8]));
        assert_eq!(window[0], 0);
    }

    #[test]
    fn preload_window_does_not_wrap_when_loop_off() {
        // At last index, loop off: only current + behind survives.
        let window = active_preload_indices(9, 10, 2, false);
        let set: HashSet<usize> = window.iter().copied().collect();
        assert_eq!(set, HashSet::from([9, 8, 7]));

        // At first index, loop off: only current + ahead survives.
        let window = active_preload_indices(0, 10, 2, false);
        let set: HashSet<usize> = window.iter().copied().collect();
        assert_eq!(set, HashSet::from([0, 1, 2]));
    }

    #[test]
    fn preload_window_full_directory_no_duplicates() {
        // 3 images, radius 5, loop on -> window is exactly {0, 1, 2}.
        let window = active_preload_indices(1, 3, 5, true);
        let mut sorted = window.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2]);
        let unique: HashSet<usize> = window.iter().copied().collect();
        assert_eq!(unique.len(), window.len(), "no duplicate indices");
    }

    #[test]
    fn preload_window_single_image_directory() {
        assert_eq!(active_preload_indices(0, 1, 0, true), vec![0]);
        assert_eq!(active_preload_indices(0, 1, 5, true), vec![0]);
        assert_eq!(active_preload_indices(0, 1, 5, false), vec![0]);
    }

    #[test]
    fn next_at_last_wraps_when_loop_on() {
        assert_eq!(step_next(9, 10, true), Some(0));
    }

    #[test]
    fn next_at_last_halts_when_loop_off() {
        assert_eq!(step_next(9, 10, false), None);
    }

    #[test]
    fn previous_at_first_wraps_when_loop_on() {
        assert_eq!(step_previous(0, 10, true), Some(9));
    }

    #[test]
    fn previous_at_first_halts_when_loop_off() {
        assert_eq!(step_previous(0, 10, false), None);
    }

    #[test]
    fn step_helpers_handle_empty_directory() {
        assert_eq!(step_next(0, 0, true), None);
        assert_eq!(step_previous(0, 0, true), None);
    }

    #[test]
    fn step_helpers_in_the_middle_ignore_loop_flag() {
        // Loop has no effect when we're not at an edge.
        assert_eq!(step_next(4, 10, true), Some(5));
        assert_eq!(step_next(4, 10, false), Some(5));
        assert_eq!(step_previous(4, 10, true), Some(3));
        assert_eq!(step_previous(4, 10, false), Some(3));
    }
}
