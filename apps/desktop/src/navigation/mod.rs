//! Image navigation: directory scanning + background preloading + LRU cache.

pub mod directory;
pub mod preloader;

use crate::diagnostics::NavigationRecord;
use crate::settings::Settings;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Coalesce window for user-initiated navigation (arrow keys, mouse wheel).
/// Events arriving within this window get summed into a single jump instead
/// of starting a decode per step — blazing through 20 wheel clicks jumps
/// directly from N to N+20 with one decode, not twenty. The value is low
/// enough that a single key press still feels immediate.
pub const NAV_DEBOUNCE: Duration = Duration::from_millis(30);

/// Format a directory index as its offset from the current image: `"N"`,
/// `"N+1"`, `"N-2"`, etc. Used in preload / cache-eviction debug logs so
/// the human reading them doesn't have to do mental arithmetic.
pub fn format_offset(index: usize, current_index: usize) -> String {
    let delta = index as i64 - current_index as i64;
    if delta == 0 {
        "N".to_string()
    } else if delta > 0 {
        format!("N+{delta}")
    } else {
        // `delta` is already negative, so `{delta}` formats with a sign.
        format!("N{delta}")
    }
}

/// Format a byte count as `"1.2 MB"` or `"42.5 KB"`. Used by cache eviction
/// debug logs.
pub fn format_bytes(bytes: usize) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.1} KB", b / 1024.0)
    }
}

/// Per-feature runtime state owned by `App`.
pub struct State {
    pub dir_list: Option<directory::DirectoryList>,
    pub preloader: Option<preloader::Preloader>,
    pub image_cache: preloader::ImageCache,
    /// Recent navigation records for performance diagnostics (newest last, cap 10).
    pub history: VecDeque<NavigationRecord>,
    /// Current image dimensions — stored so resize can update the view without
    /// needing to hit the cache.
    pub current_image_size: Option<(u32, u32)>,
    /// When false, skip eager preloading of adjacent images so only the
    /// currently displayed image consumes decode work. Useful for
    /// benchmarking single-image cold-start times. Driven by
    /// Settings → General → "Preload next/prev images".
    pub preload_neighbors: bool,
    /// Index of the image we're waiting on the preloader to finish, if any.
    /// Set when `navigate` hits a cache miss and submits the target index as
    /// the priority-zero preload task. Cleared when either a `Ready` arrives
    /// for that index (which also triggers the render) or the user navigates
    /// again (pointing us at a different target). While `Some`, the window
    /// title shows "Loading…".
    pub pending_current: Option<usize>,
    /// Direction of the last navigation — drives neighbor preload priority
    /// (`DirectoryList::preload_range`). `Unknown` at startup and after
    /// non-directional jumps (open-file, refresh, settings re-decode).
    pub last_direction: directory::Direction,
    /// Accumulator for the debounced navigation path. Each arrow key / wheel
    /// tick adds ±1. When `nav_deadline` expires, the app applies the net
    /// delta in one jump and clears this.
    pub pending_nav_delta: i32,
    /// Deadline at which the next pending nav flush fires. Extended on
    /// every incoming debounced `Navigate` so a sustained wheel spin
    /// collapses to a single jump at the end.
    pub nav_deadline: Option<Instant>,
}

impl State {
    pub fn new() -> Self {
        Self {
            dir_list: None,
            preloader: None,
            image_cache: preloader::ImageCache::new(),
            history: VecDeque::with_capacity(10),
            current_image_size: None,
            preload_neighbors: true,
            pending_current: None,
            last_direction: directory::Direction::Unknown,
            pending_nav_delta: 0,
            nav_deadline: None,
        }
    }

    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            preload_neighbors: settings.preload_neighbors,
            ..Self::new()
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}
