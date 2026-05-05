//! Sort comparators for the directory file list.
//!
//! All comparators are ascending; Name is the tiebreaker for Date and
//! FileType so equal-key runs stay deterministic. We use `sort_by` (not
//! `sort_unstable_by`) so equal-key order is preserved across re-sorts.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortBy {
    #[default]
    Name,
    Date,
    FileType,
}

/// Sort `files` in place by the chosen column.
pub fn sort_files(files: &mut [PathBuf], sort_by: SortBy) {
    match sort_by {
        SortBy::Name => files.sort_by(|a, b| compare_name(a, b)),
        SortBy::Date => files.sort_by(|a, b| compare_date(a, b)),
        SortBy::FileType => files.sort_by(|a, b| compare_file_type(a, b)),
    }
}

/// Natural, case-insensitive name compare. `photo_2 < photo_10` (not
/// alphabetic order) — the whole point of using `alphanumeric-sort`.
fn compare_name(a: &Path, b: &Path) -> Ordering {
    let a_name = a
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let b_name = b
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    alphanumeric_sort::compare_str(&a_name, &b_name)
}

fn mtime(path: &Path) -> Option<SystemTime> {
    path.metadata().ok()?.modified().ok()
}

/// Date primary, Name tiebreaker. Files with no readable mtime sort first
/// so they're easy to spot rather than hidden at the end.
fn compare_date(a: &Path, b: &Path) -> Ordering {
    match mtime(a).cmp(&mtime(b)) {
        Ordering::Equal => compare_name(a, b),
        other => other,
    }
}

/// Lowercased extension primary, Name tiebreaker. Empty extension sorts
/// first (a `README` shows up before `a.jpg`).
fn compare_file_type(a: &Path, b: &Path) -> Ordering {
    let a_ext = a
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    let b_ext = b
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default();
    match a_ext.cmp(&b_ext) {
        Ordering::Equal => compare_name(a, b),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::time::Duration;

    fn make_files(dir: &Path, names: &[&str]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for name in names {
            let p = dir.join(name);
            fs::write(&p, b"x").unwrap();
            out.push(p);
        }
        out
    }

    fn names_of(files: &[PathBuf]) -> Vec<String> {
        files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn name_natural_order_beats_alphabetic() {
        // The bug-fix golden case: "photo_10" must not sort before "photo_2".
        let dir = tempfile::tempdir().unwrap();
        let mut files = make_files(
            dir.path(),
            &["photo_10.jpg", "photo_2.jpg", "photo_11.jpg", "photo_1.jpg"],
        );
        sort_files(&mut files, SortBy::Name);
        assert_eq!(
            names_of(&files),
            vec!["photo_1.jpg", "photo_2.jpg", "photo_10.jpg", "photo_11.jpg",]
        );
    }

    #[test]
    fn name_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = make_files(dir.path(), &["Cherry.jpg", "Apple.jpg", "banana.jpg"]);
        sort_files(&mut files, SortBy::Name);
        assert_eq!(
            names_of(&files),
            vec!["Apple.jpg", "banana.jpg", "Cherry.jpg"]
        );
    }

    #[test]
    fn file_type_primary_with_name_tiebreaker() {
        // Ascending extension order means jpg < png. Within jpg, b < c.
        let dir = tempfile::tempdir().unwrap();
        let mut files = make_files(dir.path(), &["b.jpg", "a.png", "c.jpg"]);
        sort_files(&mut files, SortBy::FileType);
        assert_eq!(names_of(&files), vec!["b.jpg", "c.jpg", "a.png"]);
    }

    #[test]
    fn file_type_empty_extension_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = make_files(dir.path(), &["a.jpg", "README"]);
        sort_files(&mut files, SortBy::FileType);
        assert_eq!(names_of(&files), vec!["README", "a.jpg"]);
    }

    #[test]
    fn date_primary_with_name_tiebreaker() {
        // Set explicit mtimes via `File::set_modified` (stable since 1.75) so
        // the test is deterministic across filesystems / CI clocks. The Name
        // tiebreaker is exercised by giving two files the same mtime.
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("z_old.jpg");
        let newer_a = dir.path().join("b_new.jpg");
        let newer_b = dir.path().join("a_new.jpg");
        for p in [&older, &newer_a, &newer_b] {
            fs::write(p, b"x").unwrap();
        }
        let t_old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let t_new = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);
        File::options()
            .write(true)
            .open(&older)
            .unwrap()
            .set_modified(t_old)
            .unwrap();
        File::options()
            .write(true)
            .open(&newer_a)
            .unwrap()
            .set_modified(t_new)
            .unwrap();
        File::options()
            .write(true)
            .open(&newer_b)
            .unwrap()
            .set_modified(t_new)
            .unwrap();

        let mut files = vec![newer_a.clone(), older.clone(), newer_b.clone()];
        sort_files(&mut files, SortBy::Date);
        // Older first; among the two newer (equal mtime), Name tiebreaker → a < b.
        assert_eq!(
            names_of(&files),
            vec!["z_old.jpg", "a_new.jpg", "b_new.jpg"]
        );
    }

    #[test]
    fn already_sorted_slice_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = make_files(dir.path(), &["a.jpg", "b.jpg", "c.jpg"]);
        let before = files.clone();
        sort_files(&mut files, SortBy::Name);
        assert_eq!(files, before);
    }

    #[test]
    fn empty_and_single_element_dont_panic() {
        let mut empty: Vec<PathBuf> = Vec::new();
        sort_files(&mut empty, SortBy::Name);
        sort_files(&mut empty, SortBy::Date);
        sort_files(&mut empty, SortBy::FileType);

        let dir = tempfile::tempdir().unwrap();
        let mut one = make_files(dir.path(), &["only.jpg"]);
        sort_files(&mut one, SortBy::Name);
        sort_files(&mut one, SortBy::Date);
        sort_files(&mut one, SortBy::FileType);
        assert_eq!(names_of(&one), vec!["only.jpg"]);
    }
}
