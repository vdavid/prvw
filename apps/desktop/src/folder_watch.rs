//! Filesystem watching for live folder sync.
//!
//! A `FolderWatcher` owns a `notify` `RecommendedWatcher` (macOS FSEvents) over a **dynamic set of
//! non-recursive paths** — we watch the specific folders whose contents are on screen, never the
//! whole disk. `watch(path)` / `unwatch(path)` re-target the set as the active folder changes.
//!
//! Raw `notify` events are noisy: a single editor save fires temp-write + rename + remove, and a
//! bulk copy fires hundreds of creates. So a dedicated coalescer thread debounces them
//! (`COALESCE_WINDOW`, ~150 ms of quiet) and emits ONE `AppCommand::FolderChanged` per affected
//! folder. Adds/removes are left for the consumer's re-scan to discover (robust against
//! rename-saves); only `Modify`-flagged paths ride along in `modified` so a re-saved image
//! re-decodes. The post crosses to the main thread via the global `EventLoopProxy`, never blocking
//! it. No tokio — `std::thread` + channels, the same pattern as `navigation::preloader`.
//!
//! The debounce/coalesce logic is the pure, headless-tested `Coalescer`; the thread is a thin
//! timing shell around it.
//!
//! Watching is asynchronous: `watch(path)` queues a request and the thread applies it. Because
//! FSEvents only reports changes made after its stream starts, "requested" and "armed" are
//! different states, and a change made in between is lost. So the thread posts
//! `AppCommand::WatchedFoldersChanged` with the set it has actually applied, which the QA state
//! exposes as `watched_folders`: the barrier the live-sync E2E tests wait on before touching a
//! folder.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{Event, EventKind, RecursiveMode, Watcher};
use winit::event_loop::EventLoopProxy;

use crate::commands::AppCommand;

/// Quiet period after the last raw event before a folder's coalesced change is emitted. Long
/// enough to fold an editor's temp-write-rename save and a burst of bulk-copy events into one
/// re-scan, short enough that a live add/delete feels immediate.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(150);

/// One coalesced filesystem change for a single folder, ready to post to the main thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderChange {
    /// The watched folder that changed.
    pub folder: PathBuf,
    /// Paths inside `folder` that were flagged `Modify` (content changed in place). The consumer
    /// evicts these from its caches and re-decodes them. Sorted + deduped. Adds/removes are NOT
    /// here — the consumer's re-scan discovers those.
    pub modified: Vec<PathBuf>,
}

/// A raw filesystem event reduced to what the coalescer needs: the affected path and whether it
/// was an in-place content modification. Produced from `notify::Event`; a separate type so the
/// coalescer is testable without constructing `notify` events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEvent {
    pub path: PathBuf,
    /// True for `EventKind::Modify(_)` (content/metadata change). Creates, removes, and renames are
    /// false — they're discovered by the re-scan, so the coalescer only needs to know they touched
    /// the folder.
    pub is_modify: bool,
}

/// Pure debounce/coalesce core. Ingest raw events as they arrive (`ingest`); call `flush` after a
/// quiet period to drain one `FolderChange` per affected folder. No timing inside — the owning
/// thread decides when to flush, which keeps this unit-testable.
#[derive(Debug, Default)]
pub struct Coalescer {
    /// Per-folder accumulator: the set of `Modify`-flagged paths seen since the last flush. A
    /// folder present here (even with an empty set) has a pending change to emit. `BTreeMap` keeps
    /// the flush order deterministic, which the tests rely on.
    pending: BTreeMap<PathBuf, std::collections::BTreeSet<PathBuf>>,
}

impl Coalescer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one raw event into the pending set. The event's parent directory is the affected
    /// folder; a `Modify` also records the path so the consumer can targeted-reload it. Events
    /// whose path has no parent are ignored (can't attribute them to a watched folder).
    pub fn ingest(&mut self, event: RawEvent) {
        let Some(folder) = event.path.parent().map(Path::to_path_buf) else {
            return;
        };
        let modified = self.pending.entry(folder).or_default();
        if event.is_modify {
            modified.insert(event.path);
        }
    }

    /// True when there's nothing pending — the owning thread parks on the event channel instead of
    /// spinning on a timer.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drain all pending changes into one `FolderChange` per folder and reset. Called by the owning
    /// thread once the coalesce window has elapsed with no new events.
    pub fn flush(&mut self) -> Vec<FolderChange> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(folder, modified)| FolderChange {
                folder,
                modified: modified.into_iter().collect(),
            })
            .collect()
    }
}

/// Translate a `notify::Event` into zero or more `RawEvent`s (one per affected path). Returns an
/// empty vec for event kinds we don't care about. Pure so it's testable.
fn raw_events_from(event: &Event) -> Vec<RawEvent> {
    // `notify` emits Access events on plain reads; ignore them — they don't change folder contents.
    if matches!(event.kind, EventKind::Access(_)) {
        return Vec::new();
    }
    let is_modify = matches!(event.kind, EventKind::Modify(_));
    event
        .paths
        .iter()
        .map(|p| RawEvent {
            path: p.clone(),
            is_modify,
        })
        .collect()
}

/// Owns the `notify` watcher and the coalescer thread. Holding the struct keeps both alive; drop it
/// (on app exit) to stop watching and end the worker.
pub struct FolderWatcher {
    /// Commands to the watcher thread: watch/unwatch a path. Kept separate from the notify event
    /// channel so the consumer can re-target the watch set without racing the event stream.
    control_tx: mpsc::Sender<Control>,
}

enum Control {
    Watch(PathBuf),
    Unwatch(PathBuf),
}

impl FolderWatcher {
    /// Start the watcher. Spawns one OS thread that owns the `notify::RecommendedWatcher`, applies
    /// watch/unwatch requests, and coalesces raw events into `AppCommand::FolderChanged` posts via
    /// `proxy`. Returns `None` if the platform watcher can't be created (logged; the app keeps
    /// running without live sync).
    pub fn start(proxy: EventLoopProxy<AppCommand>) -> Option<Self> {
        let (control_tx, control_rx) = mpsc::channel::<Control>();
        // notify delivers events on its own internal thread into this channel.
        let (event_tx, event_rx) = mpsc::channel::<notify::Result<Event>>();

        let watcher = match notify::recommended_watcher(move |res| {
            // If the receiver is gone the app is shutting down; drop the event.
            let _ = event_tx.send(res);
        }) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("Folder watcher unavailable, live sync disabled: {e}");
                return None;
            }
        };

        std::thread::Builder::new()
            .name("prvw-folder-watch".into())
            .spawn(move || watch_loop(watcher, control_rx, event_rx, proxy))
            .expect("Failed to spawn folder watcher thread");
        log::info!("Folder watcher started (dedicated OS thread)");
        Some(Self { control_tx })
    }

    /// Start watching `folder` (non-recursive). Idempotent at the `notify` level (re-watching a
    /// path is a no-op there). Fire-and-forget.
    pub fn watch(&self, folder: PathBuf) {
        if self.control_tx.send(Control::Watch(folder)).is_err() {
            log::debug!("Folder watcher worker is gone — dropping watch request");
        }
    }

    /// Stop watching `folder`. Fire-and-forget; unwatching a path that isn't watched is harmless.
    pub fn unwatch(&self, folder: PathBuf) {
        if self.control_tx.send(Control::Unwatch(folder)).is_err() {
            log::debug!("Folder watcher worker is gone — dropping unwatch request");
        }
    }
}

/// The watcher thread body. Owns the `notify` watcher (so its lifetime matches the thread), applies
/// control messages, and drives the coalescer: park until an event arrives, then keep draining with
/// a `COALESCE_WINDOW` timeout until the burst goes quiet, then flush + post.
fn watch_loop(
    mut watcher: notify::RecommendedWatcher,
    control_rx: mpsc::Receiver<Control>,
    event_rx: mpsc::Receiver<notify::Result<Event>>,
    proxy: EventLoopProxy<AppCommand>,
) {
    let mut coalescer = Coalescer::new();
    // Folders whose `notify` watch is applied and live, mirrored to the main thread whenever it
    // changes. `notify`'s macOS backend returns from `watch` only once the FSEvents stream covering
    // the path has started, so membership here means events are actually flowing.
    let mut armed = BTreeSet::new();
    loop {
        // Always service pending control messages first so a re-target takes effect promptly.
        drain_control(&mut watcher, &control_rx, &mut armed, &proxy);

        let recv = if coalescer.is_empty() {
            // Nothing buffered — block until the next event (or a control nudge wakes us via the
            // short timeout below). A long timeout keeps the thread idle (respect-resources).
            event_rx.recv_timeout(Duration::from_millis(250))
        } else {
            // Mid-burst — wait only for the quiet window; on timeout, flush.
            event_rx.recv_timeout(COALESCE_WINDOW)
        };

        match recv {
            Ok(Ok(event)) => {
                for raw in raw_events_from(&event) {
                    coalescer.ingest(raw);
                }
            }
            Ok(Err(e)) => log::debug!("Folder watch event error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !coalescer.is_empty() {
                    for change in coalescer.flush() {
                        log::debug!(
                            "Folder changed: {} ({} modified)",
                            change.folder.display(),
                            change.modified.len()
                        );
                        if proxy
                            .send_event(AppCommand::FolderChanged {
                                folder: change.folder,
                                modified: change.modified,
                            })
                            .is_err()
                        {
                            // Event loop closed — app is exiting.
                            return;
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // notify's sender dropped (watcher gone) — nothing more will arrive.
                log::debug!("Folder watcher event channel closed — worker exiting");
                return;
            }
        }
    }
}

/// Apply every queued watch/unwatch request. Errors are logged, not fatal — a folder that vanished
/// between request and apply just fails to watch.
fn drain_control(
    watcher: &mut notify::RecommendedWatcher,
    control_rx: &mpsc::Receiver<Control>,
    armed: &mut BTreeSet<PathBuf>,
    proxy: &EventLoopProxy<AppCommand>,
) {
    let mut changed = false;
    while let Ok(request) = control_rx.try_recv() {
        let applied = match &request {
            Control::Watch(path) => match watcher.watch(path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    log::debug!("Watching {}", path.display());
                    true
                }
                Err(e) => {
                    log::debug!("Failed to watch {}: {e}", path.display());
                    false
                }
            },
            Control::Unwatch(path) => match watcher.unwatch(path) {
                Ok(()) => {
                    log::debug!("Unwatched {}", path.display());
                    true
                }
                Err(e) => {
                    log::debug!("Failed to unwatch {}: {e}", path.display());
                    false
                }
            },
        };
        changed |= record_watch_outcome(armed, request, applied);
    }
    if changed {
        let _ = proxy.send_event(AppCommand::WatchedFoldersChanged {
            folders: armed.iter().cloned().collect(),
        });
    }
}

/// Fold one applied watch/unwatch request into the set of folders whose watch is live. Returns
/// true when membership changed, so the caller posts only when the answer moved.
///
/// Exactly one case arms a folder: a `Watch` that `notify` accepted. Everything else disarms it,
/// including a **failed** `Watch` on a folder that was already armed — `notify`'s macOS backend
/// tears down and rebuilds one FSEvents stream over the whole path set on every call, so a
/// rejected re-watch (the folder was deleted and recreated, say) can leave the previous stream
/// gone. `armed` answers "are events flowing for this folder", never "did we ask for them": it
/// feeds `/state`'s `watched_folders`, which is the barrier the live-sync E2E tests block on
/// before they mutate a folder. A folder listed there with no live stream would wave a test
/// straight into the race the barrier exists to close.
fn record_watch_outcome(armed: &mut BTreeSet<PathBuf>, request: Control, applied: bool) -> bool {
    match request {
        Control::Watch(path) if applied => armed.insert(path),
        Control::Watch(path) | Control::Unwatch(path) => armed.remove(&path),
    }
}

/// A background folder re-lister for live folder sync. Owns one OS thread that lists a folder's
/// supported images off the main thread (a slow SMB folder must never block the event loop) and
/// posts them back as `AppCommand::ActiveFolderRescanned`. Mirrors `browser::grid_listing` but
/// posts via the `EventLoopProxy` directly so it's cross-platform (the grid lister's
/// `send_command` path is macOS-only). Newest request wins, so a burst of coalesced changes lists
/// the folder once.
pub struct RescanLister {
    request_tx: mpsc::Sender<PathBuf>,
}

impl RescanLister {
    /// Spawn the rescan worker. Runs until the `Sender` (held by `App`) drops.
    pub fn start(proxy: EventLoopProxy<AppCommand>) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<PathBuf>();
        std::thread::Builder::new()
            .name("prvw-rescan".into())
            .spawn(move || {
                while let Ok(mut folder) = request_rx.recv() {
                    // Coalesce: skip to the newest queued folder.
                    while let Ok(newer) = request_rx.try_recv() {
                        folder = newer;
                    }
                    let images = list_supported_images(&folder);
                    log::debug!(
                        "Active folder re-scanned: {} ({} image(s))",
                        folder.display(),
                        images.len()
                    );
                    if proxy
                        .send_event(AppCommand::ActiveFolderRescanned { folder, images })
                        .is_err()
                    {
                        return; // Event loop closed.
                    }
                }
            })
            .expect("Failed to spawn rescan lister worker thread");
        log::info!("Rescan lister started (dedicated OS thread)");
        RescanLister { request_tx }
    }

    /// Enqueue a folder re-scan. Fire-and-forget; the result arrives as
    /// `AppCommand::ActiveFolderRescanned`.
    pub fn list(&self, folder: PathBuf) {
        if self.request_tx.send(folder).is_err() {
            log::debug!("Rescan lister worker is gone — dropping rescan request");
        }
    }
}

/// List the supported image files directly inside `folder` (non-recursive), unsorted. The caller
/// sorts via the active `SortBy`. Returns an empty `Vec` for an unreadable folder (which the
/// re-scan reads as "the folder emptied").
#[must_use]
pub fn list_supported_images(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(crate::decoding::is_supported_extension)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch(path: &str) -> Control {
        Control::Watch(PathBuf::from(path))
    }

    fn unwatch(path: &str) -> Control {
        Control::Unwatch(PathBuf::from(path))
    }

    fn armed_set(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_successful_watch_arms_a_folder_once() {
        let mut armed = BTreeSet::new();
        assert!(record_watch_outcome(&mut armed, watch("/a"), true));
        assert_eq!(armed, armed_set(&["/a"]));
        // Re-arming an already-armed folder isn't a change, so no needless post.
        assert!(!record_watch_outcome(&mut armed, watch("/a"), true));
        assert_eq!(armed, armed_set(&["/a"]));
    }

    /// The case this function exists for. `notify` rebuilds one FSEvents stream over the whole
    /// path set on every call, so a re-watch that fails can leave the folder with no stream at
    /// all. Reporting it as armed would tell a live-sync test the barrier had cleared when it
    /// hadn't, which is the exact flake `watched_folders` was added to prevent.
    #[test]
    fn a_failed_re_watch_disarms_the_folder_it_was_already_watching() {
        let mut armed = armed_set(&["/a", "/b"]);
        assert!(record_watch_outcome(&mut armed, watch("/a"), false));
        assert_eq!(
            armed,
            armed_set(&["/b"]),
            "/a is no longer delivering events"
        );
    }

    #[test]
    fn a_failed_watch_on_an_unarmed_folder_changes_nothing() {
        let mut armed = armed_set(&["/b"]);
        assert!(!record_watch_outcome(&mut armed, watch("/a"), false));
        assert_eq!(armed, armed_set(&["/b"]));
    }

    /// Unwatch disarms whether or not `notify` accepted it: a folder we failed to unwatch
    /// (already gone, or never watched) isn't delivering events either.
    #[test]
    fn unwatch_disarms_however_it_went() {
        for applied in [true, false] {
            let mut armed = armed_set(&["/a", "/b"]);
            assert!(record_watch_outcome(&mut armed, unwatch("/a"), applied));
            assert_eq!(armed, armed_set(&["/b"]));
            assert!(!record_watch_outcome(&mut armed, unwatch("/a"), applied));
        }
    }

    /// `record_watch_outcome` is only honest if `notify` actually reports a bad path as an error
    /// rather than accepting it silently. Pin that, because the whole barrier rests on it.
    #[test]
    fn notify_rejects_a_watch_on_a_missing_folder() {
        let (tx, _rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .expect("watcher");
        let missing = std::env::temp_dir().join("prvw-folder-watch-does-not-exist");
        assert!(
            !missing.exists(),
            "the test's premise: {} is absent",
            missing.display()
        );
        assert!(
            watcher
                .watch(&missing, RecursiveMode::NonRecursive)
                .is_err(),
            "notify must reject a missing path, or `armed` would list a dead watch"
        );
    }

    fn modify(path: &str) -> RawEvent {
        RawEvent {
            path: PathBuf::from(path),
            is_modify: true,
        }
    }

    fn touch(path: &str) -> RawEvent {
        RawEvent {
            path: PathBuf::from(path),
            is_modify: false,
        }
    }

    #[test]
    fn empty_coalescer_flushes_nothing() {
        let mut c = Coalescer::new();
        assert!(c.is_empty());
        assert!(c.flush().is_empty());
    }

    #[test]
    fn coalesces_a_burst_in_one_folder_to_one_change() {
        // An editor's temp-write-rename save plus a couple of touches in the same folder collapse
        // to ONE FolderChange.
        let mut c = Coalescer::new();
        c.ingest(touch("/photos/.tmp.jpg"));
        c.ingest(touch("/photos/a.jpg"));
        c.ingest(modify("/photos/a.jpg"));
        assert!(!c.is_empty());
        let changes = c.flush();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].folder, PathBuf::from("/photos"));
        // Only the Modify-flagged path rides along.
        assert_eq!(changes[0].modified, vec![PathBuf::from("/photos/a.jpg")]);
    }

    #[test]
    fn separate_folders_produce_separate_changes() {
        let mut c = Coalescer::new();
        c.ingest(touch("/photos/a.jpg"));
        c.ingest(modify("/docs/b.png"));
        let changes = c.flush();
        assert_eq!(changes.len(), 2);
        // BTreeMap order: "/docs" before "/photos".
        assert_eq!(changes[0].folder, PathBuf::from("/docs"));
        assert_eq!(changes[0].modified, vec![PathBuf::from("/docs/b.png")]);
        assert_eq!(changes[1].folder, PathBuf::from("/photos"));
        assert!(changes[1].modified.is_empty());
    }

    #[test]
    fn add_or_remove_only_event_has_no_modified_but_still_flags_the_folder() {
        // A create or delete (is_modify=false) still reports the folder so the consumer re-scans;
        // it just carries no `modified` paths (the re-scan discovers the add/remove).
        let mut c = Coalescer::new();
        c.ingest(touch("/photos/new.jpg"));
        let changes = c.flush();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].folder, PathBuf::from("/photos"));
        assert!(changes[0].modified.is_empty());
    }

    #[test]
    fn repeated_modifies_to_same_path_dedupe() {
        let mut c = Coalescer::new();
        c.ingest(modify("/photos/a.jpg"));
        c.ingest(modify("/photos/a.jpg"));
        c.ingest(modify("/photos/a.jpg"));
        let changes = c.flush();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].modified, vec![PathBuf::from("/photos/a.jpg")]);
    }

    #[test]
    fn flush_resets_state() {
        let mut c = Coalescer::new();
        c.ingest(modify("/photos/a.jpg"));
        let _ = c.flush();
        assert!(c.is_empty());
        assert!(c.flush().is_empty());
    }

    #[test]
    fn event_with_no_parent_is_ignored() {
        let mut c = Coalescer::new();
        c.ingest(modify("/"));
        // "/" has no parent — nothing to attribute.
        assert!(c.is_empty());
    }

    #[test]
    fn access_events_are_dropped() {
        use notify::event::{AccessKind, EventKind};
        let event = Event {
            kind: EventKind::Access(AccessKind::Read),
            paths: vec![PathBuf::from("/photos/a.jpg")],
            attrs: Default::default(),
        };
        assert!(raw_events_from(&event).is_empty());
    }

    #[test]
    fn modify_event_maps_to_modify_raw_events() {
        use notify::event::{EventKind, ModifyKind};
        let event = Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: vec![PathBuf::from("/photos/a.jpg")],
            attrs: Default::default(),
        };
        let raws = raw_events_from(&event);
        assert_eq!(raws.len(), 1);
        assert!(raws[0].is_modify);
        assert_eq!(raws[0].path, PathBuf::from("/photos/a.jpg"));
    }

    #[test]
    fn create_event_maps_to_non_modify_raw_events() {
        use notify::event::{CreateKind, EventKind};
        let event = Event {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![PathBuf::from("/photos/new.jpg")],
            attrs: Default::default(),
        };
        let raws = raw_events_from(&event);
        assert_eq!(raws.len(), 1);
        assert!(!raws[0].is_modify);
    }
}
