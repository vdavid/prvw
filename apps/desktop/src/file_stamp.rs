//! Whether a file's content changed since we last read it, told from its size and modification
//! time.
//!
//! The folder watcher reports a file changed in place, but not how: FSEvents coalesces its flags,
//! so an event that says "extended attribute" can hide a content write in the same burst, and
//! Windows reports every in-place change as an unspecific modify. So nothing here trusts the event
//! kind. Every artifact the app builds from a file's bytes (a decode, a grid thumbnail) carries
//! the [`FileStamp`] taken when those bytes were read, and when the watcher reports the file, it
//! stats it again: the same size and mtime means only its metadata moved (a Finder tag, a
//! `chmod`), and whatever we built from it is still good.
//!
//! **The stamp is taken before the read, never after.** A write that lands between the read and a
//! later stat would leave a stamp newer than the bytes we hold, and the next comparison would call
//! the file unchanged. Taken before, the worst a racing write can do is make a fresh artifact look
//! stale, which costs one extra decode.
//!
//! **What this can't see:** a content write that keeps the size and lands in the same mtime tick.
//! APFS and NTFS keep nanosecond or 100 ns times, so on the disks Prvw reads that means a write
//! within the same instant. HFS+ and FAT keep whole seconds, where a same-size rewrite within the
//! second reads as unchanged until the next write.

use std::fs::Metadata;
use std::path::Path;
use std::time::SystemTime;

/// A file's size and modification time at one moment: enough to tell a content change from a
/// metadata-only one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    len: u64,
    /// `None` on a filesystem that doesn't report one, where the size alone has to do.
    modified: Option<SystemTime>,
}

impl FileStamp {
    /// The stamp of metadata already in hand, typically from the open file handle about to be read.
    pub fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }

    /// Stat `path` now. `None` when the file is gone or won't answer.
    pub fn read(path: &Path) -> Option<Self> {
        std::fs::metadata(path)
            .ok()
            .map(|m| Self::from_metadata(&m))
    }

    #[cfg(test)]
    pub fn new(len: u64, modified: Option<SystemTime>) -> Self {
        Self { len, modified }
    }
}

/// What a reported change means for what we built from the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Every artifact we hold was made from bytes the file still has: only its metadata changed.
    Unchanged,
    /// Some artifact was made from other bytes, or the file is gone or unreadable.
    Changed,
    /// We hold nothing stamped for this file, so nothing tells us. Callers treat it as `Changed`,
    /// because an unstamped cache (a preview, its dimensions) may still hold the old content.
    Unknown,
}

/// Compare the stamps our artifacts carry against the file's stamp `now`.
pub fn freshness(recorded: &[FileStamp], now: Option<FileStamp>) -> Freshness {
    let Some(now) = now else {
        return Freshness::Changed;
    };
    if recorded.is_empty() {
        Freshness::Unknown
    } else if recorded.iter().all(|stamp| *stamp == now) {
        Freshness::Unchanged
    } else {
        Freshness::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn same_size_and_mtime_is_a_metadata_only_change() {
        let stamp = FileStamp::new(1_000, at(5));
        assert_eq!(freshness(&[stamp], Some(stamp)), Freshness::Unchanged);
    }

    #[test]
    fn a_new_mtime_or_size_is_a_content_change() {
        let recorded = FileStamp::new(1_000, at(5));
        assert_eq!(
            freshness(&[recorded], Some(FileStamp::new(1_000, at(6)))),
            Freshness::Changed,
            "touched"
        );
        assert_eq!(
            freshness(&[recorded], Some(FileStamp::new(1_001, at(5)))),
            Freshness::Changed,
            "grew within the same tick"
        );
    }

    /// Two artifacts built at different times: if either is from other bytes, it's stale.
    #[test]
    fn one_stale_artifact_makes_the_file_changed() {
        let now = FileStamp::new(1_000, at(6));
        let older = FileStamp::new(900, at(5));
        assert_eq!(freshness(&[now, older], Some(now)), Freshness::Changed);
        assert_eq!(freshness(&[now, now], Some(now)), Freshness::Unchanged);
    }

    #[test]
    fn a_file_that_wont_stat_is_changed() {
        let recorded = FileStamp::new(1_000, at(5));
        assert_eq!(freshness(&[recorded], None), Freshness::Changed);
        assert_eq!(freshness(&[], None), Freshness::Changed);
    }

    #[test]
    fn nothing_recorded_is_unknown() {
        assert_eq!(
            freshness(&[], Some(FileStamp::new(1, at(1)))),
            Freshness::Unknown
        );
    }

    /// The property the whole module rests on: a Finder tag write (an extended attribute) moves
    /// neither the size nor the mtime.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_extended_attribute_write_keeps_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        std::fs::write(&path, b"not really a png").unwrap();
        let before = FileStamp::read(&path).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let status = std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.metadata:_kMDItemUserTags", "x"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(FileStamp::read(&path), Some(before));

        // And a content write does move it.
        std::fs::write(&path, b"now it's different").unwrap();
        assert_ne!(FileStamp::read(&path), Some(before));
    }
}
