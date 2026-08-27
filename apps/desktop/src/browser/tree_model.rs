//! Pure, headless-testable logic behind the browse-mode folder tree.
//!
//! The `NSOutlineView` data source (in `outline.rs`) is the macOS view wiring; this module
//! holds the platform-free decisions it leans on so they're unit-testable without AppKit:
//!
//! - [`child_directories`]: a node's child folders — directories only, dot-folders and
//!   unreadable entries skipped, sorted case-insensitively by name. Runs on the background
//!   scanner thread (never the main thread — slow filesystems would freeze the UI).
//! - [`enumerate_roots`]: the source-list roots — the home folder plus every mounted volume.
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
/// **Keyed through [`crate::paths::PathPolicy`], never on the `PathBuf`.** A `HashMap<PathBuf, _>`
/// hashes the bytes, and one folder reaches this cache spelled two ways: a scan is requested for
/// the tree row's path (`C:\Users`, from a drive enumeration) while the reveal walk asks about the
/// canonicalized one (`\\?\C:\Users`). A byte-keyed map misses, so the walk waits forever for
/// children that already arrived and the tree never leaves the drive root.
///
/// State machine per path: absent (`NotLoaded`) → [`ChildState::InFlight`] (via [`begin_scan`])
/// → [`ChildState::Loaded`] (via [`complete_scan`]). A path is scanned at most once: `begin_scan`
/// returns `false` if the path is already in flight or loaded, so the data source won't enqueue
/// duplicate scans no matter how often the outline view re-queries it during layout.
///
/// [`begin_scan`]: ChildCache::begin_scan
/// [`complete_scan`]: ChildCache::complete_scan
#[derive(Debug)]
pub struct ChildCache {
    /// How this platform decides two paths name one folder. Every map below is keyed through it.
    policy: crate::paths::PathPolicy,
    states: HashMap<std::ffi::OsString, ChildState>,
    /// When each in-flight scan started, so the overlay timer knows if one is overdue. Cleared
    /// for a path when its scan completes.
    started: HashMap<std::ffi::OsString, Instant>,
}

impl Default for ChildCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ChildCache {
    #[must_use]
    pub fn new() -> Self {
        Self::under(crate::paths::PathPolicy::HOST)
    }

    /// A cache under a named path policy, so a Mac can assert what the Windows tree will do.
    #[must_use]
    pub fn under(policy: crate::paths::PathPolicy) -> Self {
        Self {
            policy,
            states: HashMap::new(),
            started: HashMap::new(),
        }
    }

    /// The current load state of `path`, or `None` if it hasn't been scanned yet (`NotLoaded`).
    /// Test-only inspector: production code reads through [`loaded`](Self::loaded) (and treats a
    /// miss/in-flight identically — assume expandable, serve no children).
    #[cfg(test)]
    #[must_use]
    pub fn state(&self, path: &Path) -> Option<&ChildState> {
        self.states.get(&self.policy.key(path))
    }

    /// Loaded children for `path`, or `None` if not yet `Loaded` (absent or in flight).
    #[must_use]
    pub fn loaded(&self, path: &Path) -> Option<&[PathBuf]> {
        match self.states.get(&self.policy.key(path)) {
            Some(ChildState::Loaded(children)) => Some(children),
            _ => None,
        }
    }

    /// Try to start a scan for `path`. Marks it [`ChildState::InFlight`] and returns `true` when
    /// the caller should enqueue a background scan. Returns `false` (and does nothing) when a scan
    /// is already in flight or the path is already loaded — so the same path is scanned only once.
    /// `now` is the scan's start time, recorded for the overlay timer.
    pub fn begin_scan(&mut self, path: &Path, now: Instant) -> bool {
        let key = self.policy.key(path);
        if self.states.contains_key(&key) {
            return false;
        }
        self.states.insert(key.clone(), ChildState::InFlight);
        self.started.insert(key, now);
        true
    }

    /// Record a finished scan: store the children and flip the path to [`ChildState::Loaded`].
    /// Clears its in-flight start time so it no longer counts toward the overlay timer.
    pub fn complete_scan(&mut self, path: &Path, children: Vec<PathBuf>) {
        let key = self.policy.key(path);
        self.states
            .insert(key.clone(), ChildState::Loaded(children));
        self.started.remove(&key);
    }

    /// Forget `path`'s load state so the next query re-scans it from scratch. Used by live folder
    /// sync (Part B): when a watched (expanded) tree folder changes on disk, its cached child list
    /// is stale, so we invalidate it and re-request a scan. Clears any in-flight start time too.
    /// A no-op for a path that was never cached.
    pub fn invalidate(&mut self, path: &Path) {
        let key = self.policy.key(path);
        self.states.remove(&key);
        self.started.remove(&key);
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

/// A background directory scanner. Owns a single `std::thread` (the same pattern as
/// `navigation::preloader`: an OS thread + an `mpsc` channel, no tokio) that reads directories so
/// the main thread never blocks on a slow filesystem. Each request is a path; the worker computes
/// its child directories and posts them back to the main thread via the global `EventLoopProxy`
/// as `AppCommand::BrowseTreeChildrenLoaded`.
///
/// Requests are served in order and never coalesced: expanding three nodes in a row has to fill
/// three of them, and the newest is not the only one anybody is waiting on. That's the one
/// difference from `grid_listing::FolderLister`, which coalesces because only the folder the
/// user settled on is worth listing.
///
/// Only where there's a tree to fill: it posts an `AppCommand`, and the platforms with no
/// browser have no such command to post.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct TreeScanner {
    request_tx: std::sync::mpsc::Sender<ScanRequest>,
}

/// One directory for the scanner to read, and the one child it must list whatever its
/// attributes say. See [`child_directories_revealing`].
#[cfg(any(target_os = "macos", target_os = "windows"))]
struct ScanRequest {
    path: PathBuf,
    reveal_child: Option<PathBuf>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl TreeScanner {
    /// Spawn the scanner worker. It runs until the `Sender` (held by the tree, alive for the
    /// window's life) drops, closing the channel and ending the loop.
    #[must_use]
    pub fn start() -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::channel::<ScanRequest>();
        std::thread::Builder::new()
            .name("prvw-tree-scan".into())
            .spawn(move || {
                while let Ok(ScanRequest { path, reveal_child }) = request_rx.recv() {
                    let children = child_directories(&path, reveal_child.as_deref());
                    log::debug!(
                        "Tree scan done: {} ({} subdir(s))",
                        path.display(),
                        children.len()
                    );
                    // Post back to the main thread. `send_command` uses the global proxy set in
                    // `resumed()`; if it's gone the app is shutting down and we just drop the work.
                    crate::commands::send_command(
                        crate::commands::AppCommand::BrowseTreeChildrenLoaded { path, children },
                    );
                }
                log::debug!("Tree scanner worker exiting");
            })
            .expect("Failed to spawn tree scanner worker thread");
        log::info!("Tree scanner started (dedicated OS thread)");
        TreeScanner { request_tx }
    }

    /// Enqueue a directory scan. Fire-and-forget; the result comes back as an `AppCommand`.
    pub fn scan(&self, path: PathBuf) {
        self.enqueue(path, None);
    }

    /// Enqueue a scan that a reveal walk is waiting on, naming the child it has to find.
    ///
    /// Windows-only in practice: it exists because `AppData` carries the hidden attribute, and
    /// on macOS the same filter only skips dot-folders, which no reveal chain runs through.
    ///
    /// The tree hides what Explorer hides, and a reveal target can sit under a folder on that
    /// list: every Windows temp directory is under `AppData`, which carries the hidden
    /// attribute. Refusing to list the one folder the walk needs would leave the reveal stuck
    /// forever with no row to expand, so a folder the user is being taken to is listed however
    /// hidden it is. Everything else in the same directory still obeys the filter.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn scan_revealing(&self, path: PathBuf, reveal_child: PathBuf) {
        self.enqueue(path, Some(reveal_child));
    }

    fn enqueue(&self, path: PathBuf, reveal_child: Option<PathBuf>) {
        if self
            .request_tx
            .send(ScanRequest { path, reveal_child })
            .is_err()
        {
            log::warn!("Tree scanner worker is gone — dropping scan request");
        }
    }
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

/// The ordered chain of paths to walk to reveal `target` in the tree, from the containing root
/// down to (and including) `target` itself.
///
/// Browse-open positioning needs to expand the tree from the right root through every ancestor
/// directory down to the current image's folder, then select it. This computes that path list,
/// platform-free so it's unit-testable:
///
/// - The **root** is the [`Root`] whose `path` is a prefix of `target` (an ancestor or `target`
///   itself). When several roots match — e.g. `/` (a volume) and `/Users/dave` (home) both
///   contain `/Users/dave/Pics` — the **longest** match wins, so a path under home reveals under
///   the Home row, not under the volume. `roots` is searched as given (home-first per
///   [`build_roots`]); longest-prefix breaks the tie regardless of order.
/// - The **chain** is `[root.path, …each intermediate dir…, target]`, in root-to-target order.
///   When `target == root.path` the chain is just `[root.path]` (the row is already a top-level
///   node; only a select + scroll is needed, no expansion).
///
/// Returns `None` when no root contains `target` (the path is on no mounted root — nothing to
/// reveal; the caller leaves the tree as-is). Comparison is purely lexical on components: callers
/// pass already-canonical paths (the launch path is canonicalized; an image's parent is concrete),
/// so no disk access happens here.
#[must_use]
pub fn reveal_path_chain(roots: &[Root], target: &Path) -> Option<Vec<PathBuf>> {
    reveal_path_chain_under(crate::paths::PathPolicy::HOST, roots, target)
}

/// [`reveal_path_chain`] under a named path policy, so a Mac can assert what the Windows tree
/// will do. Every comparison and the ancestor walk itself go through `policy`; nothing here
/// reads the host's separators or case rules.
#[must_use]
pub fn reveal_path_chain_under(
    policy: crate::paths::PathPolicy,
    roots: &[Root],
    target: &Path,
) -> Option<Vec<PathBuf>> {
    // Longest-prefix root match: a path under home (`/Users/dave/...`, `C:\Users\dave\...`) must
    // reveal under the Home row, not under the `/` or `C:\` row, even though both are ancestors.
    let root = roots
        .iter()
        .filter(|r| policy.starts_with(target, &r.path))
        .max_by_key(|r| policy.component_count(&r.path))?;

    // Build [root, …ancestors…, target] by walking target's ancestors up to (and including) the
    // root, then reversing.
    let mut chain: Vec<PathBuf> = Vec::new();
    for ancestor in policy.ancestors(target) {
        // The root goes in with the row's own spelling, never the ancestor's: on Windows the
        // target is canonical (`\\?\C:\...`) while the root came from a drive enumeration
        // (`C:\`), and the caller looks rows up by path.
        if policy.same_path(&ancestor, &root.path) {
            chain.push(root.path.clone());
            break;
        }
        chain.push(ancestor);
    }
    chain.reverse();
    Some(chain)
}

// ── The reveal walk ──────────────────────────────────────────────────────────────────────────

/// A browse-open reveal in progress: the root-to-target chain, how far along it is, and the
/// budget that makes it finite.
///
/// The walk can't run synchronously, because expanding a node the tree hasn't scanned would find
/// no children. So it expands one level, stops, and the scan's delivery asks it for the next step.
/// That makes it a state machine driven by events from three sources — a scan the walk asked for,
/// a scan somebody else asked for, and a live re-scan that can delete the rows underneath it —
/// which is exactly the shape where "it obviously terminates" stops being obvious. So the walk
/// lives here, pure, and the Win32 side does nothing but carry out one [`RevealStep`] at a time.
///
/// ## Why it terminates
///
/// **Every call to [`next`](Self::next) spends one unit of `budget`, and `budget` is set once from
/// the chain's length and never replenished.** When it runs out the walk answers
/// [`RevealStep::GiveUp`] and keeps answering it. So the number of decisions one walk can ever
/// make is bounded by a constant, whatever the tree answers and however many events arrive — a
/// hostile or merely confused tree can make the walk end early, never make it run forever.
///
/// Two finer guarantees hold inside that bound, and they are what make the budget a backstop
/// rather than the mechanism:
///
/// - The caller's own loop only continues on [`RevealStep::Expand`], which strictly increases
///   `position`; every other step ends the loop. So one delivery costs at most `chain.len()`
///   decisions.
/// - A step whose row is missing is re-scanned at most once, because `retried_at` records the
///   position and a second miss there gives up.
///
/// The budget is generous enough that ordinary use never reaches it: a full walk down an
/// eight-deep chain spends about twenty-five decisions of a hundred.
/// Only the Windows tree drives this today; the macOS outline view still has a reveal walk of its
/// own inside `outline.rs`. Bringing that one here is worth doing and isn't this change.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Debug)]
pub struct RevealWalk {
    chain: Vec<PathBuf>,
    position: usize,
    /// The position whose missing row has already been re-scanned for, so a folder that genuinely
    /// isn't there ends the walk instead of re-scanning its parent forever.
    retried_at: Option<usize>,
    budget: u32,
}

/// What the tree can answer about the step a walk is on. Gathered in one read so a walk never
/// sees a half-changed tree, and so the Win32 side takes its state borrow exactly once per step.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepFacts {
    /// Does the tree have a row for this step's folder?
    pub has_row: bool,
    /// Has this folder's own child scan landed?
    pub children_loaded: bool,
    /// Has the scan of the step above this one landed? `false` when there is no step above.
    pub parent_children_loaded: bool,
}

/// What the tree should do next for a walk. Each one is a single Win32 act, so the caller stays a
/// `match` with no decisions of its own.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevealStep {
    /// Expand this row, then ask again — its children are known, so the walk carries straight on.
    Expand(PathBuf),
    /// Expand this row and stop. Expanding is what starts its scan, and the delivery asks again.
    ExpandAndWait(PathBuf),
    /// Select this row. The walk is over; the caller drops it.
    Select(PathBuf),
    /// Re-scan `parent`, naming `child`, because `child` has no row though `parent`'s scan landed.
    /// A folder scanned before this walk existed never got the walk's step named, so a hidden one
    /// was filtered out; naming it is the one thing that can still produce the row.
    Rescan { parent: PathBuf, child: PathBuf },
    /// Nothing to do until a delivery arrives.
    Wait,
    /// The walk can't finish. The caller drops it, so `reveal_pending` goes false and whatever is
    /// waiting on it stops waiting.
    GiveUp { path: PathBuf, why: &'static str },
}

/// How many decisions a walk gets per chain entry, plus a fixed allowance. Ordinary use spends
/// about three per entry (advance, wait for the delivery, and the odd re-scan).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const BUDGET_PER_STEP: u32 = 8;
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const BUDGET_ALLOWANCE: u32 = 32;

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
impl RevealWalk {
    /// Start a walk down `chain`, which runs from the containing root to the target folder.
    #[must_use]
    pub fn new(chain: Vec<PathBuf>) -> Self {
        let budget = u32::try_from(chain.len())
            .unwrap_or(u32::MAX / BUDGET_PER_STEP)
            .saturating_mul(BUDGET_PER_STEP)
            .saturating_add(BUDGET_ALLOWANCE);
        Self {
            chain,
            position: 0,
            retried_at: None,
            budget,
        }
    }

    /// The folder this step is about, which is what the caller reads the tree for.
    #[must_use]
    pub fn target(&self) -> Option<&Path> {
        self.chain.get(self.position).map(PathBuf::as_path)
    }

    /// The step above this one, whose scan is what produces this step's row.
    #[must_use]
    pub fn parent(&self) -> Option<&Path> {
        self.position
            .checked_sub(1)
            .and_then(|above| self.chain.get(above))
            .map(PathBuf::as_path)
    }

    /// The step after `folder`, when the walk is sitting on `folder`. A scan of `folder` has to
    /// list that one child however hidden it is (see [`TreeScanner::scan_revealing`]), or the walk
    /// is stranded with no row to expand.
    #[must_use]
    pub fn child_after(&self, folder: &Path) -> Option<PathBuf> {
        self.child_after_under(crate::paths::PathPolicy::HOST, folder)
    }

    /// [`child_after`](Self::child_after) under a named policy, so a Mac can assert the Windows
    /// answer.
    #[must_use]
    pub fn child_after_under(
        &self,
        policy: crate::paths::PathPolicy,
        folder: &Path,
    ) -> Option<PathBuf> {
        let here = self.target()?;
        policy
            .same_path(here, folder)
            .then(|| self.chain.get(self.position + 1).cloned())
            .flatten()
    }

    /// Decide the next thing to do. See the type's termination note: this spends budget on every
    /// call, including the ones that answer `Wait`.
    pub fn next(&mut self, facts: StepFacts) -> RevealStep {
        let path = self.target().unwrap_or(Path::new("")).to_path_buf();
        if self.budget == 0 {
            return RevealStep::GiveUp {
                path,
                why: "the walk ran out of steps",
            };
        }
        self.budget -= 1;
        if self.position >= self.chain.len() {
            return RevealStep::GiveUp {
                path,
                why: "the walk ran off the end of its chain",
            };
        }

        if !facts.has_row {
            let Some(parent) = self.parent().map(Path::to_path_buf) else {
                // The root's own row isn't on yet; the roots are still being put on.
                return RevealStep::Wait;
            };
            if !facts.parent_children_loaded {
                // The scan that will produce this row is still running.
                return RevealStep::Wait;
            }
            if self.retried_at == Some(self.position) {
                return RevealStep::GiveUp {
                    path,
                    why: "its parent lists no such folder",
                };
            }
            self.retried_at = Some(self.position);
            return RevealStep::Rescan {
                parent,
                child: path,
            };
        }

        if self.position + 1 == self.chain.len() {
            return RevealStep::Select(path);
        }
        if facts.children_loaded {
            self.position += 1;
            RevealStep::Expand(path)
        } else {
            RevealStep::ExpandAndWait(path)
        }
    }
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
/// `reveal_child` is the next step of a reveal walk, and it is listed however hidden it is.
/// Without that exception the walk stalls on any hidden ancestor — on Windows that is every temp
/// folder, since they all live under `AppData` — and the tree sits on a folder the user was just
/// taken to with no row for it. Naming the one folder the user is going to is far narrower than
/// showing hidden folders generally, which would put `AppData` and `System Volume Information`
/// in a photo browser.
#[must_use]
pub fn child_directories(dir: &Path, reveal_child: Option<&Path>) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let revealing =
                reveal_child.is_some_and(|wanted| crate::paths::same_path(&path, wanted));
            if !revealing && is_hidden_entry(&entry, &path) {
                return None;
            }
            // Directories only. `is_dir` resolves symlinks, so dir aliases still count.
            if path.is_dir() { Some(path) } else { None }
        })
        .collect();

    sort_by_name(&mut dirs);
    dirs
}

/// Whether a directory entry is too hidden to show in the tree.
///
/// A leading dot everywhere, because `.git` and `.Trash` are clutter on every platform. On
/// Windows that isn't the convention, so the file attributes decide as well: hidden and system
/// alike, which is what keeps `AppData` and `System Volume Information` out of a photo browser.
/// We deliberately don't read Explorer's "show hidden files" setting — skipping unconditionally
/// matches both Explorer's default and the macOS behaviour.
fn is_hidden_entry(entry: &std::fs::DirEntry, path: &Path) -> bool {
    if file_name_str(path).is_some_and(|name| name.starts_with('.')) {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        // `entry.metadata()` doesn't follow the link, which is right here: a hidden symlink is
        // hidden however ordinary its target is.
        entry.metadata().is_ok_and(|metadata| {
            crate::browser::windows::roots::hidden_by_attributes(metadata.file_attributes())
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = entry;
        false
    }
}

/// Assemble the source-list roots from a home directory and the enumerated volumes.
///
/// Pure so it's unit-testable without AppKit: the macOS volume enumeration (`NSFileManager`)
/// feeds `volumes` in `outline.rs`; here we just decide the row order and labels. Home comes
/// first (the favorite), then volumes in the order given. A volume whose path equals the home
/// path is dropped (home is already its own row), and the home row is only added when `home`
/// is `Some` (it always is in practice, but a missing home dir shouldn't panic the tree).
///
/// macOS's own root assembly. Windows leads with known folders and drive letters instead, which
/// is `windows::roots::build_windows_roots`. Compiled everywhere so its tests run on every host.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

    /// A reveal walk into a hidden folder has to get a row for it, or it stalls with nothing to
    /// expand. The dot-folder stands in for Windows's hidden attribute here, because the two go
    /// through the same filter and only one of them exists on a Mac.
    #[test]
    fn a_reveal_target_is_listed_however_hidden_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join(".appdata")).unwrap();
        fs::create_dir(root.join(".other")).unwrap();
        fs::create_dir(root.join("Photos")).unwrap();

        // Without a reveal, hidden means hidden.
        let plain: Vec<_> = child_directories(root, None)
            .iter()
            .map(|p| file_name_str(p).unwrap().to_string())
            .collect();
        assert_eq!(plain, vec!["Photos"]);

        // With one, the named folder joins it — and only that one.
        let revealing: Vec<_> = child_directories(root, Some(&root.join(".appdata")))
            .iter()
            .map(|p| file_name_str(p).unwrap().to_string())
            .collect();
        assert_eq!(revealing, vec![".appdata", "Photos"]);
    }

    #[test]
    fn child_directories_keeps_only_dirs_skips_dotfolders_and_sorts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("Photos")).unwrap();
        fs::create_dir(root.join("archive")).unwrap();
        fs::create_dir(root.join(".hidden")).unwrap();
        fs::write(root.join("a_file.txt"), b"x").unwrap();
        fs::write(root.join("image.jpg"), b"x").unwrap();

        let dirs = child_directories(root, None);
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
        assert!(child_directories(missing, None).is_empty());
    }

    #[test]
    fn child_directories_natural_numeric_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for name in ["trip_10", "trip_2", "trip_1"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        let names: Vec<_> = child_directories(root, None)
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

    // ── reveal_path_chain ──

    fn roots_home_and_volume() -> Vec<Root> {
        // The realistic shape: a `/` volume (Macintosh HD) plus the home folder under it. Both are
        // ancestors of a path in home, so the longest-prefix rule must pick home.
        vec![
            Root {
                name: "Home".to_string(),
                path: PathBuf::from("/Users/dave"),
            },
            Root {
                name: "Macintosh HD".to_string(),
                path: PathBuf::from("/"),
            },
            Root {
                name: "Backup".to_string(),
                path: PathBuf::from("/Volumes/Backup"),
            },
        ]
    }

    #[test]
    fn reveal_path_chain_under_home_walks_root_to_target() {
        let roots = roots_home_and_volume();
        let chain = reveal_path_chain(&roots, Path::new("/Users/dave/Pictures/Trip/2024")).unwrap();
        // Root-to-target order, starting at the Home root (NOT the `/` volume — longest prefix).
        assert_eq!(
            chain,
            vec![
                PathBuf::from("/Users/dave"),
                PathBuf::from("/Users/dave/Pictures"),
                PathBuf::from("/Users/dave/Pictures/Trip"),
                PathBuf::from("/Users/dave/Pictures/Trip/2024"),
            ]
        );
    }

    #[test]
    fn reveal_path_chain_picks_longest_prefix_root() {
        // A path that's under both `/` and `/Users/dave` must resolve to the home root.
        let roots = roots_home_and_volume();
        let chain = reveal_path_chain(&roots, Path::new("/Users/dave/Pics")).unwrap();
        assert_eq!(chain.first(), Some(&PathBuf::from("/Users/dave")));
    }

    #[test]
    fn reveal_path_chain_under_volume_not_home_uses_the_volume() {
        // A path on a mounted volume (not under home) reveals under that volume's root.
        let roots = roots_home_and_volume();
        let chain = reveal_path_chain(&roots, Path::new("/Volumes/Backup/Photos/Old")).unwrap();
        assert_eq!(
            chain,
            vec![
                PathBuf::from("/Volumes/Backup"),
                PathBuf::from("/Volumes/Backup/Photos"),
                PathBuf::from("/Volumes/Backup/Photos/Old"),
            ]
        );
    }

    #[test]
    fn reveal_path_chain_target_equals_root_is_just_the_root() {
        // Selecting a root itself needs no expansion — the chain is the single root row.
        let roots = roots_home_and_volume();
        let chain = reveal_path_chain(&roots, Path::new("/Users/dave")).unwrap();
        assert_eq!(chain, vec![PathBuf::from("/Users/dave")]);
    }

    #[test]
    fn reveal_path_chain_no_matching_root_is_none() {
        // With no `/` volume root, a path under none of the roots has nothing to reveal. (When a
        // `/` root IS present it matches almost everything — that's the realistic Macintosh-HD
        // case; this covers the genuinely-orphaned path.)
        let roots = vec![
            Root {
                name: "Home".to_string(),
                path: PathBuf::from("/Users/dave"),
            },
            Root {
                name: "Backup".to_string(),
                path: PathBuf::from("/Volumes/Backup"),
            },
        ];
        assert_eq!(
            reveal_path_chain(&roots, Path::new("/Volumes/Other/x")),
            None
        );
    }

    /// The cases above are POSIX-shaped. What they don't cover is the shape browse mode will
    /// actually see on Windows: drive-rooted paths, the canonical `\\?\` spelling the launch
    /// argument arrives in, and a NAS share. All three run from any host, because the walk takes
    /// the policy as an argument.
    #[test]
    fn reveal_path_chain_walks_windows_paths_from_any_host() {
        let policy = crate::paths::PathPolicy::windows();
        // A drive root has to be spelled `C:\`, never a bare `C:`: `C:` is relative to that
        // drive's current directory rather than its root.
        let roots = vec![
            Root {
                name: "Pictures".to_string(),
                path: PathBuf::from(r"C:\Users\dave\Pictures"),
            },
            Root {
                name: "Local Disk (C:)".to_string(),
                path: PathBuf::from(r"C:\"),
            },
            Root {
                name: "Photos (D:)".to_string(),
                path: PathBuf::from(r"D:\"),
            },
            Root {
                name: "photos".to_string(),
                path: PathBuf::from(r"\\naspi\photos"),
            },
        ];
        let chain = |target: &str| reveal_path_chain_under(policy, &roots, Path::new(target));

        // Longest prefix wins, same as under home on macOS: the Pictures known folder beats `C:\`.
        assert_eq!(
            chain(r"C:\Users\dave\Pictures\Trip\2026").unwrap(),
            vec![
                PathBuf::from(r"C:\Users\dave\Pictures"),
                PathBuf::from(r"C:\Users\dave\Pictures\Trip"),
                PathBuf::from(r"C:\Users\dave\Pictures\Trip\2026"),
            ]
        );

        // Not under a known folder: the walk climbs to the drive row, one step per folder.
        assert_eq!(
            chain(r"C:\Program Files\Prvw").unwrap(),
            vec![
                PathBuf::from(r"C:\"),
                PathBuf::from(r"C:\Program Files"),
                PathBuf::from(r"C:\Program Files\Prvw"),
            ]
        );

        // A path on another drive reveals under that drive, never under `C:\`.
        assert_eq!(
            chain(r"D:\Photos").unwrap(),
            vec![PathBuf::from(r"D:\"), PathBuf::from(r"D:\Photos")]
        );

        // A drive root as the target is just itself: `C:\` has no parent to walk up to.
        assert_eq!(chain(r"C:\").unwrap(), vec![PathBuf::from(r"C:\")]);

        // Nothing on the tree contains it.
        assert_eq!(chain(r"E:\Photos"), None);
    }

    /// The launch path is canonicalised, so it arrives verbatim while the drive row came from a
    /// letter enumeration. The row keeps its own spelling and every step below it keeps the
    /// prefix, which is what stops a deep library from being truncated on its way back to disk.
    #[test]
    fn reveal_path_chain_matches_a_canonical_target_to_a_plain_root() {
        let policy = crate::paths::PathPolicy::windows();
        let roots = vec![Root {
            name: "Local Disk (C:)".to_string(),
            path: PathBuf::from(r"C:\"),
        }];
        assert_eq!(
            reveal_path_chain_under(policy, &roots, Path::new(r"\\?\C:\Photos\2026")).unwrap(),
            vec![
                PathBuf::from(r"C:\"),
                PathBuf::from(r"\\?\C:\Photos"),
                PathBuf::from(r"\\?\C:\Photos\2026"),
            ]
        );
    }

    /// A share's root is the server and the share name together. Climbing past it would name a
    /// machine, which no tree row lists, so the walk has to stop at `\\naspi\photos`.
    #[test]
    fn reveal_path_chain_stops_at_a_share_root() {
        let policy = crate::paths::PathPolicy::windows();
        let roots = vec![Root {
            name: "photos".to_string(),
            path: PathBuf::from(r"\\naspi\photos"),
        }];
        assert_eq!(
            reveal_path_chain_under(policy, &roots, Path::new(r"\\naspi\photos\2026\may")).unwrap(),
            vec![
                PathBuf::from(r"\\naspi\photos"),
                PathBuf::from(r"\\naspi\photos\2026"),
                PathBuf::from(r"\\naspi\photos\2026\may"),
            ]
        );
    }

    /// NTFS is case-insensitive, so the casing the user typed on the command line must still
    /// find the row the drive enumeration spelled.
    #[test]
    fn reveal_path_chain_ignores_case_on_windows() {
        let policy = crate::paths::PathPolicy::windows();
        let roots = vec![Root {
            name: "Pictures".to_string(),
            path: PathBuf::from(r"C:\Users\Dave\Pictures"),
        }];
        assert_eq!(
            reveal_path_chain_under(policy, &roots, Path::new(r"c:\users\dave\pictures\trip"))
                .unwrap(),
            vec![
                PathBuf::from(r"C:\Users\Dave\Pictures"),
                PathBuf::from(r"c:\users\dave\pictures\trip"),
            ]
        );
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

    // ── The reveal walk ──

    /// A stand-in for the Win32 treeview, so the whole walk runs from a Mac: which folders have
    /// rows, whose scans have landed, and the queue of scans the walk has asked for.
    ///
    /// `hidden` is the Windows filter this walk exists to work around — `AppData` carries the
    /// hidden attribute and every temp folder is a dot-folder — so a hidden entry appears only in
    /// a scan that names it, exactly as `child_directories` treats `reveal_child`.
    struct FakeTree {
        disk: HashMap<PathBuf, Vec<PathBuf>>,
        hidden: std::collections::HashSet<PathBuf>,
        rows: std::collections::HashSet<PathBuf>,
        loaded: std::collections::HashSet<PathBuf>,
        scans: std::collections::VecDeque<(PathBuf, Option<PathBuf>)>,
    }

    impl FakeTree {
        /// A tree of `parent → children`, with `roots` already on as rows.
        fn new(disk: &[(&str, &[&str])], roots: &[&str]) -> Self {
            Self {
                disk: disk
                    .iter()
                    .map(|(dir, kids)| {
                        (
                            PathBuf::from(dir),
                            kids.iter().map(PathBuf::from).collect::<Vec<_>>(),
                        )
                    })
                    .collect(),
                hidden: std::collections::HashSet::new(),
                rows: roots.iter().map(PathBuf::from).collect(),
                loaded: std::collections::HashSet::new(),
                scans: std::collections::VecDeque::new(),
            }
        }

        fn hiding(mut self, hidden: &[&str]) -> Self {
            self.hidden = hidden.iter().map(PathBuf::from).collect();
            self
        }

        fn facts(&self, walk: &RevealWalk) -> StepFacts {
            let path = walk.target().unwrap_or(Path::new("")).to_path_buf();
            StepFacts {
                has_row: self.rows.contains(&path),
                children_loaded: self.loaded.contains(&path),
                parent_children_loaded: walk
                    .parent()
                    .is_some_and(|above| self.loaded.contains(above)),
            }
        }

        /// Expanding a node whose scan hasn't landed asks for one, naming the walk's next step —
        /// the Win32 side does this from `TVN_ITEMEXPANDING`.
        fn expand(&mut self, path: &Path, walk: &RevealWalk) {
            if !self.loaded.contains(path) {
                self.scans
                    .push_back((path.to_path_buf(), walk.child_after(path)));
            }
        }

        /// One scan delivery: the folder's children become rows (its old ones go first, as
        /// `remove_children` does), and the folder counts as loaded.
        fn deliver(&mut self) -> bool {
            let Some((dir, named)) = self.scans.pop_front() else {
                return false;
            };
            let children: Vec<PathBuf> = self
                .disk
                .get(&dir)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|child| {
                    !self.hidden.contains(child) || named.as_deref() == Some(child.as_path())
                })
                .collect();
            let stale: Vec<PathBuf> = self
                .rows
                .iter()
                .filter(|row| row.parent() == Some(dir.as_path()))
                .cloned()
                .collect();
            for row in stale {
                self.rows.remove(&row);
            }
            self.rows.extend(children);
            self.loaded.insert(dir);
            true
        }
    }

    /// Drive a walk the way `browser::windows::ui::tree::advance_reveal` drives it, delivering one
    /// scan per round, and answer with how it ended.
    ///
    /// The round cap is the test's own safety net: a walk that doesn't finish must fail the test,
    /// never hang the suite.
    fn drive(walk: &mut RevealWalk, tree: &mut FakeTree) -> RevealStep {
        for _ in 0..500 {
            // The caller's loop: carry on only while the walk says `Expand`.
            let outcome = loop {
                match walk.next(tree.facts(walk)) {
                    RevealStep::Expand(path) => tree.expand(&path, walk),
                    RevealStep::ExpandAndWait(path) => {
                        tree.expand(&path, walk);
                        break None;
                    }
                    RevealStep::Rescan { parent, child } => {
                        tree.loaded.remove(&parent);
                        tree.scans.push_back((parent, Some(child)));
                        break None;
                    }
                    RevealStep::Wait => break None,
                    done => break Some(done),
                }
            };
            if let Some(done) = outcome {
                return done;
            }
            if !tree.deliver() {
                panic!("the walk is waiting on a scan nobody asked for");
            }
        }
        panic!("the walk never finished");
    }

    fn chain(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    /// The ordinary case, and the one the Windows browse tests exercise: a chain through a hidden
    /// ancestor and a dot-folder, both of which only a scan that names them will list.
    #[test]
    fn a_reveal_walk_reaches_its_target_through_hidden_ancestors() {
        let mut tree = FakeTree::new(
            &[
                (r"C:\", &[r"C:\Users"]),
                (r"C:\Users", &[r"C:\Users\dave"]),
                (r"C:\Users\dave", &[r"C:\Users\dave\AppData"]),
                (r"C:\Users\dave\AppData", &[r"C:\Users\dave\AppData\.tmp"]),
                (
                    r"C:\Users\dave\AppData\.tmp",
                    &[r"C:\Users\dave\AppData\.tmp\pics"],
                ),
            ],
            &[r"C:\"],
        )
        .hiding(&[r"C:\Users\dave\AppData", r"C:\Users\dave\AppData\.tmp"]);
        let mut walk = RevealWalk::new(chain(&[
            r"C:\",
            r"C:\Users",
            r"C:\Users\dave",
            r"C:\Users\dave\AppData",
            r"C:\Users\dave\AppData\.tmp",
            r"C:\Users\dave\AppData\.tmp\pics",
        ]));
        assert_eq!(
            drive(&mut walk, &mut tree),
            RevealStep::Select(PathBuf::from(r"C:\Users\dave\AppData\.tmp\pics")),
        );
    }

    /// A single-entry chain: the target is a root, so there is nothing to expand.
    #[test]
    fn a_reveal_walk_onto_a_root_selects_it_at_once() {
        let mut tree = FakeTree::new(&[], &[r"C:\"]);
        let mut walk = RevealWalk::new(chain(&[r"C:\"]));
        assert_eq!(
            drive(&mut walk, &mut tree),
            RevealStep::Select(PathBuf::from(r"C:\")),
        );
    }

    /// The folder isn't there any more. The walk asks its parent once more, naming it, and then
    /// ends — it must not keep re-scanning, and it must not stay pending for the window's life.
    #[test]
    fn a_reveal_walk_gives_up_on_a_folder_its_parent_never_lists() {
        let mut tree = FakeTree::new(&[(r"C:\", &[r"C:\Users"])], &[r"C:\"]);
        let mut walk = RevealWalk::new(chain(&[r"C:\", r"C:\gone", r"C:\gone\pics"]));
        assert!(matches!(
            drive(&mut walk, &mut tree),
            RevealStep::GiveUp { .. }
        ));
    }

    /// A live re-scan of an ancestor deletes every row under it, including the ones the walk has
    /// already passed. The walk has to cope, and above all it has to stop.
    #[test]
    fn a_reveal_walk_ends_even_when_a_re_scan_keeps_wiping_its_rows() {
        let mut tree = FakeTree::new(
            &[(r"C:\", &[r"C:\Users"]), (r"C:\Users", &[r"C:\Users\pics"])],
            &[r"C:\"],
        );
        let mut walk = RevealWalk::new(chain(&[r"C:\", r"C:\Users", r"C:\Users\pics"]));
        // Every round, something else re-scans `C:\` and the subtree goes away again.
        let mut rounds = 0;
        loop {
            rounds += 1;
            assert!(rounds < 500, "the walk never finished");
            tree.rows.retain(|row| row == Path::new(r"C:\"));
            tree.loaded.clear();
            match walk.next(tree.facts(&walk)) {
                RevealStep::Expand(path) | RevealStep::ExpandAndWait(path) => {
                    tree.expand(&path, &walk);
                    tree.deliver();
                }
                RevealStep::Rescan { parent, child } => {
                    tree.loaded.remove(&parent);
                    tree.scans.push_back((parent, Some(child)));
                    tree.deliver();
                }
                RevealStep::Wait => {
                    tree.deliver();
                }
                RevealStep::Select(_) | RevealStep::GiveUp { .. } => break,
            }
        }
    }

    /// The backstop, stated as a property rather than a scenario: whatever the tree answers, and
    /// however many times it is asked, a walk runs out of budget and then stays given up. This is
    /// the test that would have caught a walk that never terminates.
    #[test]
    fn a_reveal_walk_runs_out_of_steps_however_it_is_answered() {
        let steps = chain(&[r"C:\", r"C:\a", r"C:\a\b", r"C:\a\b\c"]);
        // Every shape of answer the tree can give, including the incoherent ones.
        for bits in 0..8u8 {
            let facts = StepFacts {
                has_row: bits & 1 != 0,
                children_loaded: bits & 2 != 0,
                parent_children_loaded: bits & 4 != 0,
            };
            let mut walk = RevealWalk::new(steps.clone());
            let mut gave_up = None;
            for call in 0..1_000 {
                if let RevealStep::GiveUp { .. } = walk.next(facts) {
                    gave_up = Some(call);
                    break;
                }
            }
            let at = gave_up.unwrap_or_else(|| panic!("{facts:?} never ran out of budget"));
            // And it stays given up rather than starting over.
            assert!(
                matches!(walk.next(facts), RevealStep::GiveUp { .. }),
                "{facts:?} un-gave-up after step {at}"
            );
        }
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

    /// The stall that left the Windows browse tree sitting on `C:\` forever: a reveal walk asks
    /// the cache about `\\?\C:\Users` (the chain is built from the canonicalized target), while
    /// the scan that filled it was requested for `C:\Users` (the row's spelling, from a drive
    /// enumeration). A `HashMap<PathBuf, _>` misses, the walk waits for a delivery that already
    /// arrived, and the tree never descends. The cache keys through the path policy instead.
    #[test]
    fn child_cache_answers_whatever_spelling_the_folder_arrives_in() {
        let mut cache = ChildCache::under(crate::paths::PathPolicy::windows());
        let scanned = Path::new(r"C:\Users");
        let revealed = Path::new(r"\\?\C:\Users");
        let t0 = Instant::now();

        assert!(cache.begin_scan(scanned, t0));
        // The walk's spelling must not start a second scan of the same folder.
        assert!(!cache.begin_scan(revealed, t0));

        let children = vec![PathBuf::from(r"C:\Users\dave")];
        cache.complete_scan(scanned, children.clone());
        assert_eq!(
            cache.loaded(revealed),
            Some(children.as_slice()),
            "the reveal walk has to see the children the scan delivered"
        );
        // And a live re-scan asked for under either spelling clears the same entry.
        cache.invalidate(revealed);
        assert_eq!(cache.loaded(scanned), None);
    }

    /// Case-sensitive platforms must not fold: two files that differ only in case are two files on
    /// a case-sensitive volume, and the tree would serve one folder's children for the other.
    #[test]
    fn child_cache_keeps_case_apart_off_windows() {
        let mut cache = ChildCache::under(crate::paths::PathPolicy::macos());
        let t0 = Instant::now();
        cache.begin_scan(Path::new("/Users/dave/Pics"), t0);
        cache.complete_scan(Path::new("/Users/dave/Pics"), Vec::new());
        assert_eq!(cache.loaded(Path::new("/Users/dave/pics")), None);
    }

    #[test]
    fn child_cache_invalidate_forces_a_re_scan() {
        // Live folder sync (Part B): a loaded folder that changed on disk must be re-scannable. After
        // `invalidate`, the path is back to NotLoaded and `begin_scan` returns true again.
        let mut cache = ChildCache::new();
        let path = Path::new("/watched/folder");
        let t0 = Instant::now();
        cache.begin_scan(path, t0);
        cache.complete_scan(path, vec![PathBuf::from("/watched/folder/sub")]);
        assert!(cache.loaded(path).is_some());
        // begin_scan on a loaded path is a no-op until we invalidate.
        assert!(!cache.begin_scan(path, t0));

        cache.invalidate(path);
        assert_eq!(cache.state(path), None);
        assert_eq!(cache.loaded(path), None);
        // Now a fresh scan can start.
        assert!(cache.begin_scan(path, t0 + Duration::from_secs(1)));
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
