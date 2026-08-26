//! How much does finding the current image in its folder cost?
//!
//! `DirectoryList::from_file` has to answer "which entry of this folder is the image we were
//! asked to open". The obvious implementation canonicalizes every entry and compares. This times
//! that against the shape Prvw ships: compare the folder once, then match names.
//!
//! Run it on the folder shapes that matter (a local disk, a network share) and on every platform,
//! because the syscall behind `canonicalize` differs wildly:
//!
//! - macOS / Linux: `realpath`, one cheap call per entry.
//! - Windows: `CreateFileW` + `GetFinalPathNameByHandleW` + `CloseHandle`, so a file open per
//!   entry, each one also passing through whatever filter drivers (Defender) are installed.
//! - Any of them over SMB: a network round trip per entry.
//!
//! ```
//! cargo run --release                    # synthetic folders of 100 / 1,000 / 5,000 files
//! cargo run --release -- /path/to/folder  # a real one, on whatever filesystem it lives
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How many times each measurement repeats. The fast variant is microseconds, so it needs
/// repeats to be measurable at all; the slow one is reported per-repeat too, for comparability.
const REPEATS: u32 = 20;

/// Folder sizes to synthesize when no folder is named. 5,000 is a year of a phone camera roll.
const SYNTHETIC_SIZES: &[usize] = &[100, 1_000, 5_000];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("Repeats per measurement: {REPEATS}\n");

    if args.is_empty() {
        let base = std::env::temp_dir().join("prvw-directory-index-bench");
        let _ = std::fs::remove_dir_all(&base);
        for &size in SYNTHETIC_SIZES {
            let folder = base.join(format!("{size}-files"));
            std::fs::create_dir_all(&folder).expect("create the synthetic folder");
            for i in 0..size {
                std::fs::write(folder.join(format!("img-{i:05}.jpg")), b"x").expect("write");
            }
            measure(&folder, &format!("synthetic, {size} files"));
        }
        let _ = std::fs::remove_dir_all(&base);
    } else {
        for arg in &args {
            measure(Path::new(arg), arg);
        }
    }
}

fn measure(folder: &Path, label: &str) {
    let mut files = list(folder);
    if files.is_empty() {
        println!("{label}: no files, skipping");
        return;
    }
    files.sort();
    // `position` short-circuits, so where the opened image sits in the sort order decides what
    // the scan costs. First is the best case, last the worst, and middle is what a Finder
    // double-click somewhere in a camera roll actually pays.
    let last = files[files.len() - 1].clone();
    let middle = files[files.len() / 2].clone();
    let first = files[0].clone();

    println!("{label} ({} files)", files.len());
    for (position, target) in [("first", &first), ("middle", &middle), ("last", &last)] {
        let canonical = target.canonicalize().expect("canonicalize the target");
        let scan = time(|| {
            std::hint::black_box(
                files
                    .iter()
                    .position(|f| f.canonicalize().ok().as_ref() == Some(&canonical)),
            );
        });
        let names = time(|| {
            std::hint::black_box(index_by_name(&files, folder, &canonical));
        });
        println!(
            "  target {position:>6}: canonicalize each {:>10} | compare names {:>10} | {:>6.0}x",
            micros(scan),
            micros(names),
            scan.as_secs_f64() / names.as_secs_f64().max(f64::MIN_POSITIVE)
        );
    }
    println!();
}

/// What Prvw does: settle the folder once, then compare file names.
fn index_by_name(files: &[PathBuf], dir: &Path, canonical_target: &Path) -> Option<usize> {
    let target_folder = canonical_target.parent()?;
    let same_folder =
        dir == target_folder || dir.canonicalize().is_ok_and(|d| d == target_folder);
    if !same_folder {
        return None;
    }
    files
        .iter()
        .position(|f| f.file_name() == canonical_target.file_name())
}

fn list(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect()
}

fn time(mut body: impl FnMut()) -> Duration {
    // One untimed pass so the directory's metadata is in the OS cache for both variants alike.
    body();
    let start = Instant::now();
    for _ in 0..REPEATS {
        body();
    }
    start.elapsed() / REPEATS
}

fn micros(d: Duration) -> String {
    format!("{:.1} µs", d.as_secs_f64() * 1e6)
}
