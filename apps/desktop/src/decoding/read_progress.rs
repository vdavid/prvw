//! Live byte progress of one file read, shared between the reading thread and the main thread.
//!
//! [`read_file_cancellable`](super::read_file_cancellable) sets the total from the file's metadata,
//! then bumps the byte count once per [`READ_CHUNK_BYTES`] chunk. The main thread reads
//! [`ReadProgress::fraction`] about 10 times a second to fill the bar under the "Loading…" overlay.
//! Everything is relaxed atomics: the counter is only ever shown to a person, so a chunk of skew
//! costs nothing.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Env var that pauses the file read by this many milliseconds before every chunk, so integration
/// tests can watch the progress bar climb on a file that would otherwise read instantly. Unset in
/// normal use; read once per file, not per chunk. Same family as `PRVW_SCAN_DELAY_MS`.
pub const READ_DELAY_ENV_VAR: &str = "PRVW_READ_DELAY_MS";

/// The `PRVW_READ_DELAY_MS` test delay, if set to a valid millisecond count.
#[must_use]
pub fn read_delay() -> Option<Duration> {
    std::env::var(READ_DELAY_ENV_VAR)
        .ok()?
        .parse::<u64>()
        .ok()
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
}

/// How much the file read pulls per `read` call. Big enough that a local file costs a handful of
/// syscalls, small enough that a 5 MB file on a share moves the bar ~20 times on the way through.
pub const READ_CHUNK_BYTES: usize = 256 * 1024;

/// Stands in for "we haven't learned the file's length yet", which is different from a zero-length
/// file (that one is instantly complete).
const TOTAL_UNKNOWN: u64 = u64::MAX;

/// A file read's byte progress. Cheap to clone behind an `Arc`; every method is safe to call from
/// either side.
#[derive(Debug)]
pub struct ReadProgress {
    bytes_read: AtomicU64,
    total_bytes: AtomicU64,
    done: AtomicBool,
}

impl Default for ReadProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadProgress {
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes_read: AtomicU64::new(0),
            total_bytes: AtomicU64::new(TOTAL_UNKNOWN),
            done: AtomicBool::new(false),
        }
    }

    /// Record the file's length, read from its metadata before the first chunk. A read whose
    /// metadata call fails never calls this, so the bar stays hidden rather than lying.
    pub fn set_total(&self, total: u64) {
        self.total_bytes.store(total, Ordering::Relaxed);
    }

    /// Add the bytes one `read` call returned.
    pub fn add(&self, bytes: u64) {
        self.bytes_read.fetch_add(bytes, Ordering::Relaxed);
    }

    /// The whole file is in memory. The bar holds at full from here through the decode, which has
    /// no honest progress of its own and is short next to the read on a slow share.
    pub fn finish(&self) {
        self.done.store(true, Ordering::Relaxed);
    }

    /// Bytes read so far. The app draws [`fraction`](Self::fraction) instead; this is the raw
    /// count the tests assert a read lands exactly on.
    #[cfg(test)]
    #[must_use]
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// The file's length, or `None` while it's still unknown.
    #[must_use]
    pub fn total_bytes(&self) -> Option<u64> {
        match self.total_bytes.load(Ordering::Relaxed) {
            TOTAL_UNKNOWN => None,
            total => Some(total),
        }
    }

    /// How full to draw the bar, in `0.0..=1.0`. `None` while the file's length is unknown, which
    /// is the caller's cue to draw no bar at all.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        if self.done.load(Ordering::Relaxed) {
            return Some(1.0);
        }
        let total = self.total_bytes()?;
        if total == 0 {
            return Some(1.0);
        }
        let read = self.bytes_read.load(Ordering::Relaxed);
        Some((read as f64 / total as f64).clamp(0.0, 1.0) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_stays_hidden_until_the_file_length_is_known() {
        let progress = ReadProgress::new();
        assert_eq!(progress.total_bytes(), None);
        assert_eq!(progress.fraction(), None, "no total, no honest bar to draw");
    }

    #[test]
    fn the_fraction_climbs_with_the_bytes_read() {
        let progress = ReadProgress::new();
        progress.set_total(1_000);
        assert_eq!(progress.fraction(), Some(0.0));

        progress.add(250);
        assert_eq!(progress.fraction(), Some(0.25));
        progress.add(250);
        assert_eq!(progress.fraction(), Some(0.5));
        progress.add(500);
        assert_eq!(progress.fraction(), Some(1.0));
        assert_eq!(progress.bytes_read(), 1_000);
    }

    #[test]
    fn an_empty_file_reads_as_complete() {
        let progress = ReadProgress::new();
        progress.set_total(0);
        assert_eq!(
            progress.fraction(),
            Some(1.0),
            "nothing to read means nothing left to wait for"
        );
    }

    #[test]
    fn a_finished_read_holds_the_bar_full_through_the_decode() {
        let progress = ReadProgress::new();
        progress.set_total(1_000);
        progress.add(400);
        progress.finish();
        assert_eq!(progress.fraction(), Some(1.0));
    }

    #[test]
    fn a_file_that_grew_under_us_still_stops_at_full() {
        let progress = ReadProgress::new();
        progress.set_total(100);
        progress.add(180);
        assert_eq!(
            progress.fraction(),
            Some(1.0),
            "clamped, never past the end"
        );
    }
}
