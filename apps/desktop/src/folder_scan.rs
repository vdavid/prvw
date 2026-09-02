//! The one folder scanner every consumer shares.
//!
//! Reading a directory can take a minute on a stale SMB mount, so nothing in the app ever calls
//! `read_dir` on the main thread. One dedicated `std::thread` (an OS thread + channels, the same
//! shape as `navigation::preloader` — no rayon, no tokio) does every folder read, and a single
//! `AppCommand::FolderScanned` carries the result back to the main thread, where
//! `App::handle_folder_scanned` routes it to whoever asked: image mode's `DirectoryList`, the
//! browse grid, the browse tree's child rows, and live folder sync's diff.
//!
//! One `read_dir` pass yields **both** the supported images and the child directories, so a folder
//! that image mode and the tree both care about is read once, not twice.
//!
//! - **Dedupe.** A request for a folder that's already queued is dropped. A request for the folder
//!   currently being read sets a re-run flag instead, so a change that lands mid-scan is picked up
//!   by a fresh pass right after — never missed, never doubled. Which requests name one folder is
//!   [`crate::paths::PathPolicy`]'s call, never a byte comparison: on Windows a launch argument, a
//!   canonicalized path, and a drive enumeration spell the same folder three ways.
//! - **Progress.** Each running scan owns an `Arc<AtomicUsize>` bumped per directory entry, plus
//!   its start `Instant`. The main thread reads both by folder path ([`FolderScanner::progress`]),
//!   which is how overlays show "3,412 images so far" and apply their appearance delay.
//! - **Test hook.** `PRVW_SCAN_DELAY_MS` makes every scan sleep that long before reading, so
//!   integration tests can hold the app in its scan-pending state deterministically. Same family
//!   as `PRVW_BACKGROUND_WINDOW`.
//!
//! The queue logic is the pure, headless-tested [`ScanQueue`]; the worker is a thin shell around
//! it.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

use crate::commands::AppCommand;
use crate::paths::PathPolicy;

/// Env var that delays every scan by the given number of milliseconds. Integration tests set it to
/// hold the app in its scan-pending state long enough to assert on. Unset in normal use.
pub const SCAN_DELAY_ENV_VAR: &str = "PRVW_SCAN_DELAY_MS";

/// What one `read_dir` pass found, both lists **unsorted** — consumers order them themselves
/// (image mode and the grid by the active `SortBy`, the tree by name).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FolderContents {
    /// Files whose extension `decoding::is_supported_extension` accepts. Directories are excluded,
    /// so a folder named `holiday.jpg` doesn't show up as an image.
    pub images: Vec<PathBuf>,
    /// Child directories a browse sidebar would show: what
    /// `browser::tree_model::hidden_from_the_tree` hides is already out, because deciding that
    /// needs the `DirEntry` this read produced and asking again later would cost a second `stat`
    /// per folder. A reveal walk's next step survives the filter (see
    /// [`FolderScanner::request_revealing`]). The tree is the only consumer.
    pub subdirs: Vec<PathBuf>,
}

/// One folder to read, and the one child a reveal walk needs listed however hidden it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRequest {
    /// The folder to read.
    pub folder: PathBuf,
    /// The one child the read must list however hidden it is.
    pub reveal_child: Option<PathBuf>,
}

/// A running scan's live progress, readable from the main thread while the worker reads.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    /// Directory entries seen so far. Bumped once per entry, so it climbs through a long read.
    pub entries: Arc<AtomicUsize>,
    /// When this scan started reading, for overlays that only appear after a delay.
    // Read by the scan status texts (spec section 4), which land in a follow-up.
    #[allow(dead_code)]
    pub started: Instant,
}

impl ScanProgress {
    /// Entries seen so far.
    // Read by the scan status texts (spec section 4), which land in a follow-up.
    #[allow(dead_code)]
    #[must_use]
    pub fn count(&self) -> usize {
        self.entries.load(Ordering::Relaxed)
    }
}

/// The scanner's queue: what's waiting, what's running, and whether the running scan has to run
/// again. Pure (no I/O, no threads) so the dedupe and re-run rules are unit-testable.
///
/// Every "is this the same folder?" question goes through [`PathPolicy`], and the progress map is
/// keyed through it too. A `HashMap<PathBuf, _>` hashes bytes, and on Windows one folder reaches
/// this queue spelled several ways at once.
#[derive(Debug)]
pub struct ScanQueue {
    /// How this platform decides two paths name one folder.
    policy: PathPolicy,
    queued: VecDeque<ScanRequest>,
    running: Option<PathBuf>,
    /// Set when a request arrives for the folder that's currently being read, carrying whatever
    /// reveal child that request named. The scan can't absorb a change it already passed, so we
    /// re-run it once the current pass finishes.
    rerun_running: Option<Option<PathBuf>>,
    /// Live progress per running scan, keyed by folder.
    progress: HashMap<OsString, ScanProgress>,
}

impl Default for ScanQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::under(PathPolicy::HOST)
    }

    /// A queue under a named path policy, so a Mac can assert what the Windows scanner will do.
    #[must_use]
    pub fn under(policy: PathPolicy) -> Self {
        Self {
            policy,
            queued: VecDeque::new(),
            running: None,
            rerun_running: None,
            progress: HashMap::new(),
        }
    }

    /// Record a scan request. Returns `true` when it added work the worker needs to wake for;
    /// `false` when the folder was already queued (the reveal child is folded into the waiting
    /// request) or already running (flagged to re-run).
    pub fn request(&mut self, folder: PathBuf, reveal_child: Option<PathBuf>) -> bool {
        if self
            .running
            .as_deref()
            .is_some_and(|running| self.policy.same_path(running, &folder))
        {
            // A re-run already asked for keeps its reveal child unless this request names one:
            // the walk that named it is still waiting, and one pass can honour both.
            let existing = self.rerun_running.take().flatten();
            self.rerun_running = Some(reveal_child.or(existing));
            return false;
        }
        if let Some(waiting) = self
            .queued
            .iter_mut()
            .find(|queued| self.policy.same_path(&queued.folder, &folder))
        {
            waiting.reveal_child = reveal_child.or_else(|| waiting.reveal_child.take());
            return false;
        }
        self.queued.push_back(ScanRequest {
            folder,
            reveal_child,
        });
        true
    }

    /// Take the next request to read, marking its folder running and opening its progress counter.
    /// `None` when nothing is queued.
    pub fn start_next(&mut self, now: Instant) -> Option<(ScanRequest, Arc<AtomicUsize>)> {
        let request = self.queued.pop_front()?;
        let entries = Arc::new(AtomicUsize::new(0));
        self.progress.insert(
            self.policy.key(&request.folder),
            ScanProgress {
                entries: Arc::clone(&entries),
                started: now,
            },
        );
        self.running = Some(request.folder.clone());
        self.rerun_running = None;
        Some((request, entries))
    }

    /// Close out the running scan. Returns `true` when a request arrived mid-scan and the folder
    /// was re-queued (at the front, so the stale view is corrected before other work).
    pub fn finish(&mut self) -> bool {
        let Some(folder) = self.running.take() else {
            return false;
        };
        self.progress.remove(&self.policy.key(&folder));
        if let Some(reveal_child) = self.rerun_running.take() {
            self.queued.push_front(ScanRequest {
                folder,
                reveal_child,
            });
            return true;
        }
        false
    }

    /// Live progress for `folder`, or `None` when no scan of it is running. A folder that's only
    /// queued has no counter yet — it reports `None` until its read starts.
    // Read by the scan status texts (spec section 4), which land in a follow-up.
    #[allow(dead_code)]
    #[must_use]
    pub fn progress(&self, folder: &Path) -> Option<ScanProgress> {
        self.progress.get(&self.policy.key(folder)).cloned()
    }

    /// Whether a scan of `folder` is queued or running.
    // Read by the scan status texts (spec section 4), which land in a follow-up.
    #[allow(dead_code)]
    #[must_use]
    pub fn is_pending(&self, folder: &Path) -> bool {
        self.running
            .as_deref()
            .is_some_and(|running| self.policy.same_path(running, folder))
            || self
                .queued
                .iter()
                .any(|queued| self.policy.same_path(&queued.folder, folder))
    }
}

/// Handle onto the shared scanner. Cheap to clone; the worker lives until the last handle drops.
#[derive(Clone)]
pub struct FolderScanner {
    /// Wakes the worker. Dropping every clone closes the channel and ends the worker loop.
    wake_tx: mpsc::Sender<()>,
    queue: Arc<Mutex<ScanQueue>>,
}

impl FolderScanner {
    /// Spawn the scanner worker. Results come back as `AppCommand::FolderScanned`.
    #[must_use]
    pub fn start(proxy: EventLoopProxy<AppCommand>) -> Self {
        let (wake_tx, wake_rx) = mpsc::channel::<()>();
        let queue = Arc::new(Mutex::new(ScanQueue::new()));
        let worker_queue = Arc::clone(&queue);
        std::thread::Builder::new()
            .name("prvw-folder-scan".into())
            .spawn(move || worker_loop(&wake_rx, &worker_queue, &proxy))
            .expect("Failed to spawn folder scanner worker thread");
        log::info!("Folder scanner started (dedicated OS thread)");
        FolderScanner { wake_tx, queue }
    }

    /// Ask for `folder` to be scanned. Fire-and-forget; the result arrives as
    /// `AppCommand::FolderScanned`. Deduped per [`ScanQueue::request`].
    pub fn request(&self, folder: PathBuf) {
        self.enqueue(folder, None);
    }

    /// Ask for `folder` to be scanned, naming the one child a reveal walk is waiting on.
    ///
    /// The tree hides what Explorer hides, and a reveal target can sit under a folder on that
    /// list: every Windows temp directory is under `AppData`, which carries the hidden attribute.
    /// Refusing to list the one folder the walk needs would leave the reveal stuck forever with no
    /// row to expand, so a folder the user is being taken to is listed however hidden it is.
    /// Everything else in the same directory still obeys the filter.
    ///
    /// Windows-only in practice: on macOS the same filter only skips dot-folders, which no reveal
    /// chain runs through.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn request_revealing(&self, folder: PathBuf, reveal_child: PathBuf) {
        self.enqueue(folder, Some(reveal_child));
    }

    fn enqueue(&self, folder: PathBuf, reveal_child: Option<PathBuf>) {
        let added = match self.queue.lock() {
            Ok(mut queue) => queue.request(folder, reveal_child),
            Err(_) => return,
        };
        if added && self.wake_tx.send(()).is_err() {
            log::debug!("Folder scanner worker is gone — dropping scan request");
        }
    }

    /// Live progress of `folder`'s running scan, if one is running.
    // Read by the scan status texts (spec section 4), which land in a follow-up.
    #[allow(dead_code)]
    #[must_use]
    pub fn progress(&self, folder: &Path) -> Option<ScanProgress> {
        self.queue.lock().ok()?.progress(folder)
    }

    /// Whether a scan of `folder` is queued or running.
    // Read by the scan status texts (spec section 4), which land in a follow-up.
    #[allow(dead_code)]
    #[must_use]
    pub fn is_pending(&self, folder: &Path) -> bool {
        self.queue
            .lock()
            .is_ok_and(|queue| queue.is_pending(folder))
    }
}

/// Drain the queue on every wake, scanning one folder at a time.
fn worker_loop(
    wake_rx: &mpsc::Receiver<()>,
    queue: &Arc<Mutex<ScanQueue>>,
    proxy: &EventLoopProxy<AppCommand>,
) {
    while wake_rx.recv().is_ok() {
        while let Some((request, entries)) = queue
            .lock()
            .ok()
            .and_then(|mut queue| queue.start_next(Instant::now()))
        {
            let folder = request.folder;
            let contents = read_folder(&folder, request.reveal_child.as_deref(), &entries);
            log::debug!(
                "Scanned {} ({} image(s), {} subdir(s))",
                folder.display(),
                contents.images.len(),
                contents.subdirs.len()
            );
            // Close the scan out BEFORE posting, so the main thread never sees a folder that's
            // both scanned and still reported as in flight. A mid-scan change re-queues here and
            // the next loop iteration picks it up.
            if let Ok(mut queue) = queue.lock() {
                queue.finish();
            }
            if proxy
                .send_event(AppCommand::FolderScanned {
                    folder,
                    images: contents.images,
                    subdirs: contents.subdirs,
                })
                .is_err()
            {
                return; // Event loop closed.
            }
        }
    }
    log::debug!("Folder scanner worker exiting");
}

/// One `read_dir` pass over `folder`, splitting entries into supported images and the child
/// directories a browse sidebar shows, and bumping `entries` per entry seen.
///
/// `DirEntry::file_type()` comes from the directory read itself on macOS, so classifying costs no
/// extra `stat`. It doesn't follow symlinks, though, and a symlinked folder should still show as a
/// folder — so a symlink (and only a symlink) falls back to `Path::is_dir`, which resolves it.
///
/// Subdirectories go through `browser::tree_model::hidden_from_the_tree` here rather than later,
/// because the answer needs the `DirEntry`. `reveal_child` is the one folder that survives it: a
/// reveal walk sent to a hidden folder has no row to expand otherwise, and on Windows every temp
/// directory sits under a hidden `AppData`.
///
/// An unreadable folder returns empty lists rather than an error: consumers read that as "nothing
/// here" (the grid's "(No images)", image mode's empty state), which is what a person sees anyway.
#[must_use]
fn read_folder(
    folder: &Path,
    reveal_child: Option<&Path>,
    entries: &AtomicUsize,
) -> FolderContents {
    if let Some(delay) = scan_delay() {
        log::debug!("Delaying scan of {} by {delay:?}", folder.display());
        std::thread::sleep(delay);
    }
    let mut contents = FolderContents::default();
    let Ok(dir) = std::fs::read_dir(folder) else {
        return contents;
    };
    for entry in dir.filter_map(Result::ok) {
        entries.fetch_add(1, Ordering::Relaxed);
        let path = entry.path();
        let is_dir = match entry.file_type() {
            Ok(file_type) if file_type.is_symlink() => path.is_dir(),
            Ok(file_type) => file_type.is_dir(),
            Err(_) => path.is_dir(),
        };
        if is_dir {
            let revealing =
                reveal_child.is_some_and(|wanted| crate::paths::same_path(&path, wanted));
            if revealing || !crate::browser::tree_model::hidden_from_the_tree(&entry, &path) {
                contents.subdirs.push(path);
            }
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(crate::decoding::is_supported_extension)
        {
            contents.images.push(path);
        }
    }
    contents
}

/// The `PRVW_SCAN_DELAY_MS` test delay, if set to a valid millisecond count.
fn scan_delay() -> Option<Duration> {
    std::env::var(SCAN_DELAY_ENV_VAR)
        .ok()?
        .parse::<u64>()
        .ok()
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        PathBuf::from("/tmp").join(name)
    }

    /// A folder the caller only wants read, with no reveal walk waiting on it.
    fn plain(queue: &mut ScanQueue, name: &str) -> bool {
        queue.request(folder(name), None)
    }

    fn next_folder(queue: &mut ScanQueue) -> Option<PathBuf> {
        queue
            .start_next(Instant::now())
            .map(|(request, _)| request.folder)
    }

    #[test]
    fn a_queued_folder_is_not_queued_twice() {
        let mut queue = ScanQueue::new();
        assert!(plain(&mut queue, "pics"), "first request adds work");
        assert!(
            !plain(&mut queue, "pics"),
            "a folder already waiting isn't queued again"
        );
        assert!(plain(&mut queue, "docs"), "a different folder does");

        assert_eq!(next_folder(&mut queue), Some(folder("pics")));
        queue.finish();
        assert_eq!(next_folder(&mut queue), Some(folder("docs")));
        queue.finish();
        assert!(next_folder(&mut queue).is_none());
    }

    #[test]
    fn a_request_during_a_scan_re_runs_it_once() {
        // A live-sync change that lands mid-scan can't be picked up by the pass already reading,
        // so the folder runs again once — exactly once, however many changes arrived.
        let mut queue = ScanQueue::new();
        plain(&mut queue, "pics");
        queue.start_next(Instant::now());

        assert!(
            !plain(&mut queue, "pics"),
            "the running folder isn't queued; it's flagged"
        );
        assert!(!plain(&mut queue, "pics"), "a second change adds no more");

        assert!(queue.finish(), "the running scan was flagged to re-run");
        assert_eq!(next_folder(&mut queue), Some(folder("pics")));
        assert!(!queue.finish(), "the re-run isn't itself flagged");
        assert!(next_folder(&mut queue).is_none());
    }

    #[test]
    fn a_re_run_jumps_ahead_of_other_queued_work() {
        let mut queue = ScanQueue::new();
        plain(&mut queue, "pics");
        queue.start_next(Instant::now());
        plain(&mut queue, "docs");
        plain(&mut queue, "pics"); // flags the re-run

        assert!(queue.finish());
        assert_eq!(
            next_folder(&mut queue),
            Some(folder("pics")),
            "the stale folder is corrected before new work starts"
        );
    }

    #[test]
    fn a_reveal_child_survives_the_dedupe() {
        // The tree asks for a folder a reveal walk needs a hidden row from, while an ordinary
        // request for the same folder is already waiting. Dropping the second request must not
        // drop what it named, or the walk sits forever with no row to expand.
        let mut queue = ScanQueue::new();
        plain(&mut queue, "pics");
        assert!(!queue.request(folder("pics"), Some(folder("pics/.appdata"))));
        let (request, _) = queue.start_next(Instant::now()).unwrap();
        assert_eq!(request.reveal_child, Some(folder("pics/.appdata")));
    }

    #[test]
    fn a_reveal_child_named_mid_scan_reaches_the_re_run() {
        let mut queue = ScanQueue::new();
        plain(&mut queue, "pics");
        queue.start_next(Instant::now());
        queue.request(folder("pics"), Some(folder("pics/.appdata")));

        assert!(queue.finish());
        let (request, _) = queue.start_next(Instant::now()).unwrap();
        assert_eq!(request.reveal_child, Some(folder("pics/.appdata")));
    }

    #[test]
    fn windows_spells_one_folder_three_ways_and_the_queue_agrees() {
        // The launch argument, the canonicalized path, and a drive enumeration all name one
        // folder. A byte-keyed queue would scan it three times and report progress for none of
        // them.
        let mut queue = ScanQueue::under(PathPolicy::windows());
        let typed = PathBuf::from(r"c:\Users\dave\pics");
        let canonical = PathBuf::from(r"\\?\C:\Users\dave\pics");
        assert!(queue.request(typed.clone(), None));
        assert!(
            !queue.request(canonical.clone(), None),
            "the canonical spelling is the same folder"
        );

        let (request, entries) = queue.start_next(Instant::now()).unwrap();
        assert_eq!(request.folder, typed);
        entries.fetch_add(7, Ordering::Relaxed);
        assert_eq!(
            queue.progress(&canonical).map(|p| p.count()),
            Some(7),
            "progress is readable under either spelling"
        );
        assert!(queue.is_pending(&canonical));
    }

    #[test]
    fn progress_is_readable_while_a_scan_runs_and_gone_after() {
        let mut queue = ScanQueue::new();
        plain(&mut queue, "pics");
        let (_, entries) = queue.start_next(Instant::now()).unwrap();

        entries.fetch_add(3, Ordering::Relaxed);
        let progress = queue.progress(&folder("pics")).expect("scan is running");
        assert_eq!(progress.count(), 3);
        assert!(queue.progress(&folder("docs")).is_none());

        queue.finish();
        assert!(
            queue.progress(&folder("pics")).is_none(),
            "a finished scan reports no progress"
        );
    }

    #[test]
    fn pending_covers_both_queued_and_running() {
        let mut queue = ScanQueue::new();
        assert!(!queue.is_pending(&folder("pics")));
        plain(&mut queue, "pics");
        assert!(
            queue.is_pending(&folder("pics")),
            "queued counts as pending"
        );
        queue.start_next(Instant::now());
        assert!(
            queue.is_pending(&folder("pics")),
            "running counts as pending"
        );
        queue.finish();
        assert!(!queue.is_pending(&folder("pics")));
    }

    #[test]
    fn one_pass_splits_images_from_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for name in ["a.jpg", "b.PNG", "c.webp", "readme.txt", "data.json"] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        std::fs::create_dir(root.join("subdir")).unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        std::fs::write(root.join("subdir").join("nested.jpg"), b"x").unwrap();
        // A directory whose name looks like an image must not list as one.
        std::fs::create_dir(root.join("holiday.jpg")).unwrap();

        let entries = AtomicUsize::new(0);
        let contents = read_folder(root, None, &entries);

        let mut images = names(&contents.images);
        images.sort();
        assert_eq!(images, vec!["a.jpg", "b.PNG", "c.webp"]);
        let mut subdirs = names(&contents.subdirs);
        subdirs.sort();
        assert_eq!(
            subdirs,
            vec!["holiday.jpg", "subdir"],
            "a dot-folder is not a sidebar row"
        );
        assert_eq!(entries.load(Ordering::Relaxed), 8, "one bump per entry");
    }

    #[test]
    fn a_reveal_target_is_listed_however_hidden_it_is() {
        // A reveal walk sent into a hidden folder has no row to expand otherwise. The dot-folder
        // stands in for Windows's hidden attribute here: both go through the same filter, and
        // only one of them exists on a Mac.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join(".appdata")).unwrap();
        std::fs::create_dir(root.join(".other")).unwrap();
        std::fs::create_dir(root.join("Photos")).unwrap();

        let plain = names(&read_folder(root, None, &AtomicUsize::new(0)).subdirs);
        assert_eq!(plain, vec!["Photos"], "hidden means hidden");

        let mut revealing =
            names(&read_folder(root, Some(&root.join(".appdata")), &AtomicUsize::new(0)).subdirs);
        revealing.sort();
        assert_eq!(
            revealing,
            vec![".appdata", "Photos"],
            "the named folder joins it, and only that one"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_folder_counts_as_a_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let real = root.join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, root.join("aliased")).unwrap();
        std::fs::write(root.join("target.png"), b"x").unwrap();
        std::os::unix::fs::symlink(root.join("target.png"), root.join("alias.png")).unwrap();

        let contents = read_folder(root, None, &AtomicUsize::new(0));

        let mut subdirs = names(&contents.subdirs);
        subdirs.sort();
        assert_eq!(subdirs, vec!["aliased", "real"]);
        let mut images = names(&contents.images);
        images.sort();
        assert_eq!(
            images,
            vec!["alias.png", "target.png"],
            "a symlink to an image is still an image"
        );
    }

    #[test]
    fn an_unreadable_folder_scans_to_nothing() {
        let contents = read_folder(
            Path::new("/no/such/folder/prvw"),
            None,
            &AtomicUsize::new(0),
        );
        assert_eq!(contents, FolderContents::default());
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }
}
