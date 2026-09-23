//! The browse grid's Finder tag dots: which cells know their tags, and the worker that reads them.
//!
//! Display only: the grid shows tags and never sets them. A folder can sit on an SMB share, where
//! each tag read is a network round trip, so the main thread never reads one. [`GridTags`] (pure,
//! tested) decides which cells need a read, following the same visible range the thumbnails do;
//! [`TagReader`] reads them one at a time on its own thread and wakes the main thread with
//! `AppCommand::BrowseTagsAvailable` when results are waiting. A folder of 5,000 images costs a
//! read per cell scrolled into view (plus the prefetch margin), never 5,000 at once.
//!
//! A cell's tags go stale when live sync reports its file changed (`GridTags::invalidate`, via
//! `App::note_tags_changed`). The old dots stay up until the fresh read lands, so a re-read never
//! flickers them off and on.
//!
//! macOS only: Finder tags are a macOS attribute, and the Windows grid has nothing to show.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use winit::event_loop::EventLoopProxy;

use crate::commands::AppCommand;
use crate::tags::TagColor;

/// What the grid knows about its cells' tags, keyed by folder index like the thumbnails.
///
/// Entries are a few bytes and there's one per cell ever scrolled past in this folder, so nothing
/// evicts them; a new listing clears the lot ([`GridTags::reset`]).
#[derive(Debug, Default)]
pub struct GridTags {
    entries: HashMap<usize, Entry>,
    /// Cells with a read queued or running.
    in_flight: HashSet<usize>,
    /// Cells invalidated while their read was already running: that read may have seen the file
    /// before the change, so its answer is drawn but not trusted.
    stale_in_flight: HashSet<usize>,
}

#[derive(Debug)]
struct Entry {
    colors: Vec<TagColor>,
    /// False once the file changed since this was read, until a fresh read lands.
    fresh: bool,
}

impl GridTags {
    /// Forget everything: a new folder, or a re-scan that renumbered the cells.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The dot colors to draw for `index` now: the last read, even if a newer one is on its way.
    /// Empty for a cell never read.
    pub fn colors(&self, index: usize) -> &[TagColor] {
        self.entries
            .get(&index)
            .map_or(&[], |entry| entry.colors.as_slice())
    }

    /// The cells in `range` that need a read, now marked in flight. Call it on every pump; a cell
    /// already read (and still fresh) or already in flight isn't returned again.
    pub fn take_requests(&mut self, range: Range<usize>) -> Vec<usize> {
        let wanted: Vec<usize> = range
            .filter(|index| {
                !self.in_flight.contains(index)
                    && self.entries.get(index).is_none_or(|entry| !entry.fresh)
            })
            .collect();
        self.in_flight.extend(&wanted);
        wanted
    }

    /// A read for `index` landed. Returns whether the colors to draw changed, so the caller
    /// repaints only the cells that need it.
    pub fn arrived(&mut self, index: usize, colors: Vec<TagColor>) -> bool {
        self.in_flight.remove(&index);
        let fresh = !self.stale_in_flight.remove(&index);
        let changed = self.colors(index) != colors.as_slice();
        self.entries.insert(index, Entry { colors, fresh });
        changed
    }

    /// `index`'s file may have new tags: read it again on the next pump. Its dots stay up until
    /// then.
    pub fn invalidate(&mut self, index: usize) {
        if self.in_flight.contains(&index) {
            self.stale_in_flight.insert(index);
        }
        if let Some(entry) = self.entries.get_mut(&index) {
            entry.fresh = false;
        }
    }
}

/// One read the grid asked for.
struct Job {
    generation: u64,
    index: usize,
    path: PathBuf,
}

/// One read's answer, waiting for the main thread.
pub struct TagsRead {
    pub generation: u64,
    pub index: usize,
    pub colors: Vec<TagColor>,
}

/// A single background thread that reads tag attributes for the grid, in the order asked.
///
/// Serial on purpose: a read is one `getxattr`, cheap on a local disk and a round trip on a
/// share, where parallel reads would only compete for the same connection.
pub struct TagReader {
    jobs: mpsc::Sender<Job>,
    done: Arc<Mutex<Vec<TagsRead>>>,
    /// The folder generation reads are still wanted for. The worker skips a job from an older
    /// one without touching the disk, so leaving a big folder doesn't finish reading it.
    generation: Arc<AtomicU64>,
}

impl TagReader {
    pub fn start() -> Self {
        let (jobs, rx) = mpsc::channel::<Job>();
        let done: Arc<Mutex<Vec<TagsRead>>> = Arc::default();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_done = Arc::clone(&done);
        let worker_generation = Arc::clone(&generation);
        std::thread::Builder::new()
            .name("prvw-gridtags".into())
            .spawn(move || {
                let proxy: EventLoopProxy<AppCommand> = crate::commands::event_loop_proxy();
                while let Ok(job) = rx.recv() {
                    if job.generation != worker_generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    let colors =
                        crate::tags::dot_colors(&crate::tags::read_tags_for_display(&job.path));
                    let was_empty = match worker_done.lock() {
                        Ok(mut done) => {
                            let was_empty = done.is_empty();
                            done.push(TagsRead {
                                generation: job.generation,
                                index: job.index,
                                colors,
                            });
                            was_empty
                        }
                        Err(_) => return,
                    };
                    // One wake per batch, not per read: the main thread drains everything
                    // queued when it runs.
                    if was_empty && proxy.send_event(AppCommand::BrowseTagsAvailable).is_err() {
                        return;
                    }
                }
            })
            .expect("Failed to spawn grid tag reader thread");
        Self {
            jobs,
            done,
            generation,
        }
    }

    /// The folder generation the grid now lists. Reads queued for an older one are skipped.
    pub fn set_generation(&self, generation: u64) {
        self.generation.store(generation, Ordering::Relaxed);
    }

    /// Queue a read of `path`, the file at `index` in the listing of `generation`.
    pub fn request(&self, generation: u64, index: usize, path: PathBuf) {
        let _ = self.jobs.send(Job {
            generation,
            index,
            path,
        });
    }

    /// Everything read since the last drain.
    pub fn drain(&self) -> Vec<TagsRead> {
        self.done
            .lock()
            .map(|mut done| std::mem::take(&mut *done))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_range_requests_each_cell_once() {
        let mut tags = GridTags::default();
        assert_eq!(tags.take_requests(0..3), vec![0, 1, 2]);
        // In flight: not asked for again, however often the grid pumps.
        assert!(tags.take_requests(0..3).is_empty());
        // Scrolling on asks only for what's new.
        assert_eq!(tags.take_requests(2..5), vec![3, 4]);
    }

    #[test]
    fn an_arrived_read_is_drawn_and_not_requested_again() {
        let mut tags = GridTags::default();
        tags.take_requests(0..1);
        assert!(tags.arrived(0, vec![TagColor::Red]));
        assert_eq!(tags.colors(0), &[TagColor::Red]);
        assert!(tags.take_requests(0..1).is_empty());
        assert!(tags.colors(1).is_empty(), "a cell never read has no dots");
    }

    /// The repaint signal: a read that says what the cell already shows needs no repaint.
    #[test]
    fn arrived_reports_whether_the_dots_changed() {
        let mut tags = GridTags::default();
        tags.take_requests(0..1);
        assert!(!tags.arrived(0, vec![]), "no tags, and none drawn");
        tags.invalidate(0);
        tags.take_requests(0..1);
        assert!(tags.arrived(0, vec![TagColor::Blue]));
        tags.invalidate(0);
        tags.take_requests(0..1);
        assert!(!tags.arrived(0, vec![TagColor::Blue]));
    }

    /// A tag change in Finder: the cell is read again, and keeps its old dots meanwhile so the
    /// re-read doesn't flicker them.
    #[test]
    fn an_invalidated_cell_is_read_again_and_keeps_its_dots_meanwhile() {
        let mut tags = GridTags::default();
        tags.take_requests(0..2);
        tags.arrived(0, vec![TagColor::Red]);
        tags.arrived(1, vec![]);
        tags.invalidate(0);
        assert_eq!(tags.colors(0), &[TagColor::Red]);
        assert_eq!(tags.take_requests(0..2), vec![0]);
        tags.arrived(0, vec![TagColor::Green]);
        assert_eq!(tags.colors(0), &[TagColor::Green]);
        assert!(tags.take_requests(0..2).is_empty());
    }

    /// The race the extra set exists for: the file changes while its read is running. That read
    /// may predate the change, so it's drawn but the cell is read once more.
    #[test]
    fn a_change_during_a_read_triggers_one_more() {
        let mut tags = GridTags::default();
        tags.take_requests(0..1);
        tags.invalidate(0);
        assert!(
            tags.take_requests(0..1).is_empty(),
            "the running read isn't doubled"
        );
        tags.arrived(0, vec![TagColor::Red]);
        assert_eq!(tags.colors(0), &[TagColor::Red]);
        assert_eq!(tags.take_requests(0..1), vec![0]);
        tags.arrived(0, vec![]);
        assert!(tags.take_requests(0..1).is_empty());
    }

    #[test]
    fn reset_forgets_everything() {
        let mut tags = GridTags::default();
        tags.take_requests(0..2);
        tags.arrived(0, vec![TagColor::Red]);
        tags.reset();
        assert!(tags.colors(0).is_empty());
        assert_eq!(tags.take_requests(0..2), vec![0, 1]);
    }

    /// Invalidating a cell that was never read is harmless: it's read when it comes into view,
    /// the way it would have been anyway.
    #[test]
    fn invalidating_an_unread_cell_does_nothing() {
        let mut tags = GridTags::default();
        tags.invalidate(7);
        assert_eq!(tags.take_requests(7..8), vec![7]);
        tags.arrived(7, vec![]);
        assert!(tags.take_requests(7..8).is_empty());
    }
}
