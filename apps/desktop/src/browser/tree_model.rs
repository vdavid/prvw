//! Pure, headless-testable logic behind the browse-mode folder tree.
//!
//! The `NSOutlineView` data source (in `outline.rs`) is the macOS view wiring; this module
//! holds the platform-free decisions it leans on so they're unit-testable without AppKit:
//!
//! - [`child_directories`]: a node's child folders — directories only, dot-folders and
//!   unreadable entries skipped, sorted case-insensitively by name. Runs on the background
//!   scanner thread (never the main thread — slow filesystems would freeze the UI).
//! - [`enumerate_roots`]: the source-list roots — the home folder plus every mounted volume.
//! - [`next_selectable_row`]: the Up/Down arrow-key target row in a flat list of visible rows.
//! - [`ChildCache`]: the per-path load-state machine (`NotLoaded` → `InFlight` → `Loaded`) the
//!   data source serves children from. The data source NEVER reads a directory inline; it only
//!   ever consults this cache and lets the background scanner fill it.
//! - [`scan_overdue`]: the "has a scan the user is waiting on been pending longer than the
//!   loading-overlay delay?" predicate, kept pure so it's unit-testable without a clock.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a directory scan must stay pending before the tree pane shows its "Loading…" overlay.
/// Fast local dirs finish well under this, so the overlay never flashes for them; only a genuinely
/// slow scan (a stale SMB share) outlives the delay and reveals the overlay. See [`scan_overdue`].
pub const LOADING_OVERLAY_DELAY: Duration = Duration::from_secs(1);

/// The load state of one tree node's child directories. The `NSOutlineView` data source serves
/// children straight from here and never touches the disk itself: a slow filesystem read on the
/// main thread would freeze winit's event loop (the whole app). A miss enqueues a background scan
/// and shows nothing yet; the scanner posts the result back and flips the entry to `Loaded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildState {
    /// A background scan is running for this path; children aren't known yet. Treated as "no
    /// children, but assume expandable" so the disclosure triangle still shows.
    InFlight,
    /// The scan finished; these are the child directories (possibly empty for a leaf folder).
    Loaded(Vec<PathBuf>),
}

/// Per-path child-directory cache with an explicit load-state machine, plus the in-flight start
/// times the loading-overlay timer reads. Lives in the data source (`outline.rs`); split out here
/// so the state transitions are unit-testable without AppKit.
///
/// State machine per path: absent (`NotLoaded`) → [`ChildState::InFlight`] (via [`begin_scan`])
/// → [`ChildState::Loaded`] (via [`complete_scan`]). A path is scanned at most once: `begin_scan`
/// returns `false` if the path is already in flight or loaded, so the data source won't enqueue
/// duplicate scans no matter how often the outline view re-queries it during layout.
///
/// [`begin_scan`]: ChildCache::begin_scan
/// [`complete_scan`]: ChildCache::complete_scan
#[derive(Debug, Default)]
pub struct ChildCache {
    states: HashMap<PathBuf, ChildState>,
    /// When each in-flight scan started, so the overlay timer knows if one is overdue. Cleared
    /// for a path when its scan completes.
    started: HashMap<PathBuf, Instant>,
}

impl ChildCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current load state of `path`, or `None` if it hasn't been scanned yet (`NotLoaded`).
    /// Test-only inspector: production code reads through [`loaded`](Self::loaded) (and treats a
    /// miss/in-flight identically — assume expandable, serve no children).
    #[cfg(test)]
    #[must_use]
    pub fn state(&self, path: &Path) -> Option<&ChildState> {
        self.states.get(path)
    }

    /// Loaded children for `path`, or `None` if not yet `Loaded` (absent or in flight).
    #[must_use]
    pub fn loaded(&self, path: &Path) -> Option<&[PathBuf]> {
        match self.states.get(path) {
            Some(ChildState::Loaded(children)) => Some(children),
            _ => None,
        }
    }

    /// Try to start a scan for `path`. Marks it [`ChildState::InFlight`] and returns `true` when
    /// the caller should enqueue a background scan. Returns `false` (and does nothing) when a scan
    /// is already in flight or the path is already loaded — so the same path is scanned only once.
    /// `now` is the scan's start time, recorded for the overlay timer.
    pub fn begin_scan(&mut self, path: &Path, now: Instant) -> bool {
        if self.states.contains_key(path) {
            return false;
        }
        self.states.insert(path.to_path_buf(), ChildState::InFlight);
        self.started.insert(path.to_path_buf(), now);
        true
    }

    /// Record a finished scan: store the children and flip the path to [`ChildState::Loaded`].
    /// Clears its in-flight start time so it no longer counts toward the overlay timer.
    pub fn complete_scan(&mut self, path: &Path, children: Vec<PathBuf>) {
        self.states
            .insert(path.to_path_buf(), ChildState::Loaded(children));
        self.started.remove(path);
    }

    /// The earliest start time among all still-in-flight scans, or `None` if none are pending.
    /// `about_to_wait` feeds this to [`scan_overdue`] to decide whether to show the overlay and
    /// when to schedule the next wakeup.
    #[must_use]
    pub fn earliest_in_flight(&self) -> Option<Instant> {
        self.started.values().copied().min()
    }
}

/// Whether a scan that started at `earliest` is overdue as of `now` — i.e. has been pending at
/// least [`LOADING_OVERLAY_DELAY`], so the tree pane should show its "Loading…" overlay. `None`
/// means no scan is in flight, so the overlay stays hidden.
#[must_use]
pub fn scan_overdue(earliest: Option<Instant>, now: Instant) -> bool {
    earliest.is_some_and(|start| now.duration_since(start) >= LOADING_OVERLAY_DELAY)
}

/// A source-list root: the home folder or a mounted volume. `name` is the row's display label
/// (the volume's localized name, or "Home" for the home folder); `path` is what gets listed
/// and expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// Display label for the row.
    pub name: String,
    /// Absolute path this root points at.
    pub path: PathBuf,
}

/// List the immediate child directories of `dir`, ready to show as tree rows.
///
/// Keeps only directories (no files), skips dot-folders (`.git`, `.Trash`, …) and entries we
/// can't read the type of, and sorts case-insensitively by file name so the order matches what
/// a person expects in a sidebar. Symlinks that point at directories are followed (via
/// `Path::is_dir`, which resolves the link) so aliased folders still show.
///
/// Returns an empty `Vec` when `dir` can't be read (permission denied, not a directory, gone).
/// That's deliberate: the tree shows the node with no children rather than erroring.
#[must_use]
pub fn child_directories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            // Skip dot-folders: clutter the sidebar and are rarely what a person browses.
            if file_name_str(&path).is_some_and(|n| n.starts_with('.')) {
                return None;
            }
            // Directories only. `is_dir` resolves symlinks, so dir aliases still count.
            if path.is_dir() { Some(path) } else { None }
        })
        .collect();

    sort_by_name(&mut dirs);
    dirs
}

/// Assemble the source-list roots from a home directory and the enumerated volumes.
///
/// Pure so it's unit-testable without AppKit: the macOS volume enumeration (`NSFileManager`)
/// feeds `volumes` in `outline.rs`; here we just decide the row order and labels. Home comes
/// first (the favorite), then volumes in the order given. A volume whose path equals the home
/// path is dropped (home is already its own row), and the home row is only added when `home`
/// is `Some` (it always is in practice, but a missing home dir shouldn't panic the tree).
#[must_use]
pub fn build_roots(home: Option<PathBuf>, volumes: Vec<Root>) -> Vec<Root> {
    let mut roots = Vec::with_capacity(volumes.len() + 1);
    let home_path = home.clone();
    if let Some(home) = home {
        roots.push(Root {
            name: "Home".to_string(),
            path: home,
        });
    }
    for volume in volumes {
        if home_path.as_deref() == Some(volume.path.as_path()) {
            continue;
        }
        roots.push(volume);
    }
    roots
}

/// The source-list roots: the home folder followed by every mounted volume.
///
/// macOS-only — enumerates volumes via `NSFileManager mountedVolumeURLs` and reads each
/// volume's localized display name. Falls back to listing `/Volumes` if the typed enumeration
/// returns nothing. The ordering/labelling lives in [`build_roots`] (pure, tested); this is the
/// platform glue that feeds it.
#[cfg(target_os = "macos")]
#[must_use]
pub fn enumerate_roots() -> Vec<Root> {
    let home = dirs_home();
    let volumes = enumerate_volumes();
    build_roots(home, volumes)
}

/// The user's home directory, via `$HOME`. `None` only if the var is unset (never, in practice).
#[cfg(target_os = "macos")]
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Enumerate mounted volumes as `(display name, path)` roots, via `NSFileManager`.
#[cfg(target_os = "macos")]
fn enumerate_volumes() -> Vec<Root> {
    use objc2_foundation::{NSFileManager, NSVolumeEnumerationOptions};

    let mut out = Vec::new();
    let fm = NSFileManager::defaultManager();
    // `mountedVolumeURLsIncludingResourceValuesForKeys:options:` with no keys and the skip-hidden
    // option returns the user-visible mounted volumes (no internal partitions). Main-thread call
    // from the outline build.
    let urls = fm.mountedVolumeURLsIncludingResourceValuesForKeys_options(
        None,
        NSVolumeEnumerationOptions::SkipHiddenVolumes,
    );
    if let Some(urls) = urls {
        for url in &urls {
            if let Some(root) = volume_root_from_url(&url) {
                out.push(root);
            }
        }
    }

    // Fallback: if the typed enumeration came back empty (rare), read /Volumes directly.
    if out.is_empty()
        && let Ok(entries) = std::fs::read_dir("/Volumes")
    {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir()
                && let Some(name) = file_name_str(&path)
            {
                out.push(Root {
                    name: name.to_string(),
                    path: path.clone(),
                });
            }
        }
        sort_by_name_roots(&mut out);
    }
    out
}

/// Build a `Root` from a volume `NSURL`: its localized display name + filesystem path.
#[cfg(target_os = "macos")]
fn volume_root_from_url(url: &objc2_foundation::NSURL) -> Option<Root> {
    let path = PathBuf::from(url.path()?.to_string());
    // Prefer the volume's localized name; fall back to the path's last component.
    let name = volume_display_name(url)
        .or_else(|| file_name_str(&path).map(str::to_string))
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    Some(Root { name, path })
}

/// Read a volume URL's localized display name (`NSURLVolumeLocalizedNameKey`).
#[cfg(target_os = "macos")]
fn volume_display_name(url: &objc2_foundation::NSURL) -> Option<String> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSString, NSURLVolumeLocalizedNameKey};

    // SAFETY: `NSURLVolumeLocalizedNameKey` yields an `NSString`, matching the `value` slot's
    // expected type (the method's documented safety requirement).
    unsafe {
        let mut value: Option<Retained<AnyObject>> = None;
        url.getResourceValue_forKey_error(&mut value, NSURLVolumeLocalizedNameKey)
            .ok()?;
        let value = value?;
        let ns: Retained<NSString> = value.downcast().ok()?;
        Some(ns.to_string())
    }
}

/// Sort roots case-insensitively by display name (used only for the `/Volumes` fallback;
/// `NSFileManager` order is kept otherwise).
#[cfg(target_os = "macos")]
fn sort_by_name_roots(roots: &mut [Root]) {
    roots.sort_by(|a, b| {
        alphanumeric_sort::compare_str(a.name.to_lowercase(), b.name.to_lowercase())
    });
}

/// The row a Up/Down arrow key should move to, given the current selection and how many rows
/// are visible. `delta` is +1 (Down) or -1 (Up). Clamps at both ends so arrowing past the edge
/// is a no-op rather than wrapping.
///
/// `selected` is `None` when nothing is selected yet — Down then selects the first row, Up the
/// last, matching how a fresh source list behaves under the arrow keys. Returns `None` only when
/// there are no rows at all.
#[must_use]
pub fn next_selectable_row(
    selected: Option<usize>,
    visible_rows: usize,
    delta: i32,
) -> Option<usize> {
    if visible_rows == 0 {
        return None;
    }
    let last = visible_rows - 1;
    match selected {
        Some(current) => {
            let next = (current as i64 + delta as i64).clamp(0, last as i64);
            Some(next as usize)
        }
        // Nothing selected: Down lands on the first row, Up on the last.
        None if delta > 0 => Some(0),
        None => Some(last),
    }
}

/// Borrow a path's final component as a `&str`, or `None` if it has none / isn't valid UTF-8.
fn file_name_str(path: &Path) -> Option<&str> {
    path.file_name().and_then(|n| n.to_str())
}

/// Sort paths case-insensitively by file name, the order a sidebar should read in.
fn sort_by_name(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| {
        let an = file_name_str(a).unwrap_or_default().to_lowercase();
        let bn = file_name_str(b).unwrap_or_default().to_lowercase();
        alphanumeric_sort::compare_str(&an, &bn)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn child_directories_keeps_only_dirs_skips_dotfolders_and_sorts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("Photos")).unwrap();
        fs::create_dir(root.join("archive")).unwrap();
        fs::create_dir(root.join(".hidden")).unwrap();
        fs::write(root.join("a_file.txt"), b"x").unwrap();
        fs::write(root.join("image.jpg"), b"x").unwrap();

        let dirs = child_directories(root);
        let names: Vec<_> = dirs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        // Files excluded, dot-folder excluded, case-insensitive name order.
        assert_eq!(names, vec!["archive", "Photos"]);
    }

    #[test]
    fn child_directories_unreadable_is_empty_not_error() {
        // A path that doesn't exist can't be read; we want an empty list, not a panic.
        let missing = Path::new("/this/path/does/not/exist/prvw-test");
        assert!(child_directories(missing).is_empty());
    }

    #[test]
    fn child_directories_natural_numeric_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for name in ["trip_10", "trip_2", "trip_1"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        let names: Vec<_> = child_directories(root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // Natural order: trip_2 before trip_10 (not alphabetic).
        assert_eq!(names, vec!["trip_1", "trip_2", "trip_10"]);
    }

    #[test]
    fn build_roots_home_first_then_volumes() {
        let home = PathBuf::from("/Users/dave");
        let volumes = vec![
            Root {
                name: "Macintosh HD".to_string(),
                path: PathBuf::from("/"),
            },
            Root {
                name: "Backup".to_string(),
                path: PathBuf::from("/Volumes/Backup"),
            },
        ];
        let roots = build_roots(Some(home), volumes);
        let labels: Vec<_> = roots.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(labels, vec!["Home", "Macintosh HD", "Backup"]);
    }

    #[test]
    fn build_roots_drops_volume_equal_to_home() {
        // A volume whose path is the home path shouldn't duplicate the Home row.
        let home = PathBuf::from("/Users/dave");
        let volumes = vec![Root {
            name: "dave".to_string(),
            path: PathBuf::from("/Users/dave"),
        }];
        let roots = build_roots(Some(home), volumes);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "Home");
    }

    #[test]
    fn build_roots_without_home_is_just_volumes() {
        let volumes = vec![Root {
            name: "Macintosh HD".to_string(),
            path: PathBuf::from("/"),
        }];
        let roots = build_roots(None, volumes);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "Macintosh HD");
    }

    #[test]
    fn next_selectable_row_clamps_at_edges() {
        // 5 rows, 0..=4.
        assert_eq!(next_selectable_row(Some(2), 5, 1), Some(3));
        assert_eq!(next_selectable_row(Some(2), 5, -1), Some(1));
        // Clamp, don't wrap.
        assert_eq!(next_selectable_row(Some(4), 5, 1), Some(4));
        assert_eq!(next_selectable_row(Some(0), 5, -1), Some(0));
    }

    #[test]
    fn next_selectable_row_from_no_selection() {
        assert_eq!(next_selectable_row(None, 5, 1), Some(0)); // Down → first
        assert_eq!(next_selectable_row(None, 5, -1), Some(4)); // Up → last
    }

    #[test]
    fn next_selectable_row_empty_list_is_none() {
        assert_eq!(next_selectable_row(None, 0, 1), None);
        assert_eq!(next_selectable_row(Some(0), 0, 1), None);
    }

    // ── ChildCache state machine ──

    #[test]
    fn child_cache_miss_then_in_flight_then_loaded() {
        let mut cache = ChildCache::new();
        let path = Path::new("/Volumes/Slow/folder");
        let t0 = Instant::now();

        // Miss: nothing known yet.
        assert_eq!(cache.state(path), None);
        assert_eq!(cache.loaded(path), None);

        // First begin_scan starts it and asks the caller to enqueue a scan.
        assert!(cache.begin_scan(path, t0));
        assert_eq!(cache.state(path), Some(&ChildState::InFlight));
        // In flight: not loaded yet, so no children to serve.
        assert_eq!(cache.loaded(path), None);

        // A second begin_scan while in flight is a no-op — never scan the same path twice.
        assert!(!cache.begin_scan(path, t0 + Duration::from_millis(5)));

        // Completion stores children and flips to Loaded.
        let children = vec![PathBuf::from("/Volumes/Slow/folder/a")];
        cache.complete_scan(path, children.clone());
        assert_eq!(
            cache.state(path),
            Some(&ChildState::Loaded(children.clone()))
        );
        assert_eq!(cache.loaded(path), Some(children.as_slice()));

        // begin_scan on an already-loaded path is also a no-op.
        assert!(!cache.begin_scan(path, t0 + Duration::from_secs(1)));
    }

    #[test]
    fn child_cache_tracks_earliest_in_flight_and_clears_on_complete() {
        let mut cache = ChildCache::new();
        let a = Path::new("/a");
        let b = Path::new("/b");
        let t0 = Instant::now();

        assert_eq!(cache.earliest_in_flight(), None);
        cache.begin_scan(a, t0);
        cache.begin_scan(b, t0 + Duration::from_millis(100));
        // The oldest still-pending scan wins.
        assert_eq!(cache.earliest_in_flight(), Some(t0));

        // Completing the oldest leaves the younger one as the new earliest.
        cache.complete_scan(a, Vec::new());
        assert_eq!(
            cache.earliest_in_flight(),
            Some(t0 + Duration::from_millis(100))
        );

        // Completing the last clears it entirely.
        cache.complete_scan(b, Vec::new());
        assert_eq!(cache.earliest_in_flight(), None);
    }

    #[test]
    fn scan_overdue_only_past_the_delay() {
        let t0 = Instant::now();
        // No scan in flight → never overdue.
        assert!(!scan_overdue(None, t0));
        // Just started → not overdue.
        assert!(!scan_overdue(Some(t0), t0));
        // Just under the threshold → not overdue.
        assert!(!scan_overdue(
            Some(t0),
            t0 + LOADING_OVERLAY_DELAY - Duration::from_millis(1)
        ));
        // At/past the threshold → overdue, show the overlay.
        assert!(scan_overdue(Some(t0), t0 + LOADING_OVERLAY_DELAY));
        assert!(scan_overdue(
            Some(t0),
            t0 + LOADING_OVERLAY_DELAY + Duration::from_secs(5)
        ));
    }
}
