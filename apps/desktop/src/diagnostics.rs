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
//! - **Process RSS via `ps`** — no platform crate, just a subprocess. Returns 0.0 on
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

/// Get the current process RSS in MB via `ps`. Returns 0.0 on failure.
pub fn get_process_rss_mb() -> f64 {
    let pid = std::process::id();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            text.trim().parse::<f64>().ok()
        })
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

/// Build human/agent-readable diagnostics text covering cache, navigation timing,
/// and memory. Called by `App::update_shared_state` every time observable state
/// changes.
pub fn build_text(
    cache_diag: &CacheDiagnostics,
    current_index: usize,
    navigation_history: &VecDeque<NavigationRecord>,
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
            let current_marker = if entry.index == current_index {
                "  ← current"
            } else {
                ""
            };
            out.push_str(&format!(
                "    [{}] {}  {}x{}  {}  decoded in {}ms{}\n",
                entry.index,
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

    // Process memory via ps
    let process_memory = get_process_rss_mb();
    out.push_str(&format!(
        "\nprocess_memory: {:.1} MB (cache: {})\n",
        process_memory,
        format_bytes(cache_diag.total_memory)
    ));

    out
}
