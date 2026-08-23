//! # Diagnostics
//!
//! Performance observability — cache state, navigation history, process RSS. Feeds the
//! `diagnostics_text` field in `SharedAppState` so the QA server and MCP clients can
//! read it.
//!
//! ## Design
//!
//! - **Pure data in, formatted string out.** `build_text(cache_diag, current_index,
//!   history)` takes everything it needs as parameters. No `impl App`, no privileged
//!   access to private fields.
//! - **`NavigationRecord` lives here** because it's a measurement type (from/to index,
//!   cache hit, duration, timestamp). The ring buffer lives on `navigation::State`;
//!   diagnostics just formats it.
//! - **Process RSS is per-platform and best-effort.** macOS shells out to `ps`, Linux
//!   reads `/proc/self/statm`, Windows asks `GetProcessMemoryInfo`. Returns 0.0 on
//!   failure. Fine because it's diagnostic output, not a gate on anything.
//!
//! ## Format
//!
//! Human-readable multi-line text. Read by:
//! - `GET /diagnostics` (QA HTTP)
//! - `prvw://diagnostics` (MCP resource)
//! - Ad-hoc log dumps

use crate::navigation::preloader::{self, CacheDiagnostics};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A record of a single navigation event, for performance diagnostics.
pub struct NavigationRecord {
    pub from_index: usize,
    pub to_index: usize,
    pub was_cached: bool,
    pub total_time: Duration,
    pub timestamp: Instant,
}

/// Format a byte count as a human-readable string (for example, "47.2 MB").
pub fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// The current process' resident set size in MB. Returns 0.0 when the platform
/// won't say, which callers render as-is: this is diagnostic output, not a gate.
pub fn get_process_rss_mb() -> f64 {
    process_rss_bytes().map_or(0.0, |bytes| bytes as f64 / (1024.0 * 1024.0))
}

/// macOS: `ps` reports RSS in KB. A subprocess is heavier than `task_info`, but
/// this runs on state changes rather than per frame, and it needs no `unsafe`.
#[cfg(target_os = "macos")]
fn process_rss_bytes() -> Option<u64> {
    let pid = std::process::id();
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let kb: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    kb.checked_mul(1024)
}

/// Linux: `/proc/self/statm`'s second field is the resident set in pages.
#[cfg(target_os = "linux")]
fn process_rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = parse_statm_resident_pages(&statm)?;
    // SAFETY: `sysconf` reads a static system value and touches no memory of
    // ours.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).ok()?;
    pages.checked_mul(page_size)
}

/// Pull the resident-pages field out of a `/proc/self/statm` line. Split out so
/// the parse is testable on any host.
#[cfg(any(target_os = "linux", test))]
fn parse_statm_resident_pages(statm: &str) -> Option<u64> {
    statm.split_whitespace().nth(1)?.parse().ok()
}

/// Windows: `PROCESS_MEMORY_COUNTERS::WorkingSetSize` is the RSS equivalent,
/// already in bytes.
#[cfg(target_os = "windows")]
fn process_rss_bytes() -> Option<u64> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?,
        ..Default::default()
    };
    // SAFETY: `GetCurrentProcess` hands back a pseudo-handle that needs no
    // closing, and `GetProcessMemoryInfo` fills our stack struct, whose size we
    // pass alongside it.
    unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) }.ok()?;
    u64::try_from(counters.WorkingSetSize).ok()
}

/// Any other platform: no reading available yet.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn process_rss_bytes() -> Option<u64> {
    None
}

/// Build human/agent-readable diagnostics text covering cache, navigation timing,
/// and memory. Called by `App::update_shared_state` every time observable state
/// changes.
pub fn build_text(
    cache_diag: &CacheDiagnostics,
    current_index: usize,
    dir_files: &[PathBuf],
    navigation_history: &VecDeque<NavigationRecord>,
    preview_bytes: usize,
) -> String {
    let mut out = String::new();

    // Cache diagnostics
    out.push_str("cache:\n");
    out.push_str(&format!(
        "  total_memory: {}\n",
        format_bytes(cache_diag.total_memory)
    ));
    out.push_str(&format!(
        "  entries: {} of {} budget\n",
        cache_diag.entries.len(),
        format_bytes(cache_diag.memory_budget)
    ));
    if !cache_diag.entries.is_empty() {
        out.push_str("  images:\n");
        for entry in &cache_diag.entries {
            let position = dir_files.iter().position(|p| p == &entry.path);
            let (index_label, current_marker) = match position {
                Some(idx) if idx == current_index => (format!("[{idx}] "), "  ← current"),
                Some(idx) => (format!("[{idx}] "), ""),
                None => (String::new(), ""),
            };
            out.push_str(&format!(
                "    {}{}  {}x{}  {}  decoded in {}ms{}\n",
                index_label,
                entry.file_name,
                entry.width,
                entry.height,
                format_bytes(entry.memory_bytes),
                entry.decode_duration.as_millis(),
                current_marker,
            ));
        }
    }

    // Preloader status
    out.push_str("\npreloader:\n");
    out.push_str(&format!(
        "  window: current ± {}\n",
        preloader::preload_count()
    ));

    // Navigation history
    out.push_str("\nrecent_navigations (newest first):\n");
    if navigation_history.is_empty() {
        out.push_str("  (none)\n");
    } else {
        let now = Instant::now();
        for record in navigation_history.iter().rev() {
            let ago = now.duration_since(record.timestamp);
            let cached_str = if record.was_cached { "yes" } else { "no " };
            out.push_str(&format!(
                "  {}→{}  cached: {}  display: {}ms  {:.1}s ago\n",
                record.from_index,
                record.to_index,
                cached_str,
                record.total_time.as_millis(),
                ago.as_secs_f64(),
            ));
        }
    }

    // Process memory via ps. Break out the two pixel caches so the gap to
    // RSS (GPU texture, in-flight decode buffers, allocator retention) is
    // visible at a glance.
    let process_memory = get_process_rss_mb();
    out.push_str(&format!(
        "\nprocess_memory: {:.1} MB (image cache: {}, previews: {})\n",
        process_memory,
        format_bytes(cache_diag.total_memory),
        format_bytes(preview_bytes)
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_picks_a_readable_unit() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    /// Each supported platform has to report something for a running process.
    /// Zero is the documented "couldn't tell" answer, and we shouldn't be
    /// hitting it on macOS, Linux, or Windows.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn process_rss_is_plausible() {
        let mb = get_process_rss_mb();
        assert!(mb > 0.0, "expected a non-zero RSS, got {mb} MB");
        assert!(mb < 1024.0 * 1024.0, "expected under 1 TB, got {mb} MB");
    }

    #[test]
    fn statm_resident_pages_is_the_second_field() {
        assert_eq!(
            parse_statm_resident_pages("5432 987 654 1 0 321 0"),
            Some(987)
        );
    }

    #[test]
    fn malformed_statm_is_none() {
        assert_eq!(parse_statm_resident_pages("5432"), None);
        assert_eq!(parse_statm_resident_pages(""), None);
        assert_eq!(parse_statm_resident_pages("5432 nonsense"), None);
    }
}
