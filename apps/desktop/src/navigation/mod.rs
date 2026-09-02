//! Image navigation: directory scanning + background preloading + LRU cache.

pub mod directory;
pub mod folder_diff;
pub mod preloader;
pub mod queued_nav;
pub mod sort;
pub mod wrap;

pub use sort::SortBy;

use crate::diagnostics::NavigationRecord;
use crate::settings::Settings;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Coalesce window for user-initiated navigation (arrow keys, mouse wheel).
/// Events arriving within this window get summed into a single jump instead
/// of starting a decode per step — blazing through 20 wheel clicks jumps
/// directly from N to N+20 with one decode, not twenty. The value is low
/// enough that a single key press still feels immediate.
pub const NAV_DEBOUNCE: Duration = Duration::from_millis(30);

/// How long the image we're waiting on may take before the centered "Loading…" overlay appears.
/// A local file decodes well inside this, so the overlay never flashes for one; only a genuinely
/// slow read (a big RAW, a network share) outlives the delay and reveals it. Sibling of the browse
/// tree's `browser::tree_model::LOADING_OVERLAY_DELAY`, which is longer because the tree can show
/// its rows meanwhile.
pub const LOADING_OVERLAY_DELAY: Duration = Duration::from_millis(150);

/// A folder scan image mode is waiting on, and what the answer is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingScan {
    /// The folder being read.
    pub folder: PathBuf,
    /// What to do when its images arrive.
    pub landing: ScanLanding,
}

/// What image mode does with a folder scan it asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanLanding {
    /// Keep the image already on screen and give it its real neighbours. The ordinary open: a
    /// provisional one-file `DirectoryList` is standing in for the folder until this lands.
    KeepOpenImage,
    /// Play the folder from its first image. A folder argument or a dropped folder on a platform
    /// with no browser, where the folder itself is the thing being opened. A folder with no images
    /// leaves whatever is on screen alone, and shows the "(No images)" empty state when that's
    /// nothing.
    PlayFromTop,
}

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
    /// The folder scan image mode is waiting on, if any. Set at launch, on an in-app open, and
    /// when a platform with no browser is handed a folder; cleared when `AppCommand::FolderScanned`
    /// installs the real list. While it's `Some`, `dir_list` is a stand-in and navigation is queued
    /// into `queued_nav` instead of moving.
    pub scan_pending: Option<PendingScan>,
    /// The move the user asked for while `scan_pending` was set, if any.
    /// `App::install_scanned_folder` resolves it against the real folder and navigates there.
    pub queued_nav: Option<queued_nav::QueuedNav>,
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
    /// When true, Next at the last image wraps to the first and Previous
    /// at the first wraps to the last. Drives both navigation steps and
    /// the preloader's active window. Toggled via Navigate → Loop
    /// navigation or the bare L key.
    pub loop_navigation: bool,
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
            scan_pending: None,
            queued_nav: None,
            last_direction: directory::Direction::Unknown,
            pending_nav_delta: 0,
            nav_deadline: None,
            loop_navigation: false,
        }
    }

    /// Record a relative move (arrow key, wheel, Next/Previous, a slideshow advance) asked for
    /// while the folder scan is still running. Steps fold together and cancel out; see
    /// [`queued_nav::with_step`].
    pub fn queue_nav_step(&mut self, delta: i32) {
        self.queued_nav = queued_nav::with_step(self.queued_nav, delta);
    }

    /// Record an absolute jump (Home / End) asked for while the folder scan is still running. It
    /// replaces whatever the arrows had walked to, the same as pressing it on a scanned folder.
    pub fn queue_nav_jump(&mut self, anchor: queued_nav::NavAnchor) {
        self.queued_nav = Some(queued_nav::QueuedNav::jump(anchor));
    }

    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            preload_neighbors: settings.preload_neighbors,
            loop_navigation: settings.loop_navigation,
            ..Self::new()
        }
    }

    /// The folder a scan is pending on, if one is. Callers that only ask "which folder?" go
    /// through this rather than reaching into [`PendingScan`].
    #[must_use]
    pub fn scan_folder(&self) -> Option<&std::path::Path> {
        self.scan_pending
            .as_ref()
            .map(|pending| pending.folder.as_path())
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}
