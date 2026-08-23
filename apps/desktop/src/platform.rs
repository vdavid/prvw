//! Platform-specific integrations.
//!
//! Per-platform submodules live under `platform::<os>` and are gated with `#[cfg]`.
//! When a second platform lands, mirror the `macos/` shape with its own submodule.
//! Small per-OS queries that don't warrant a submodule live here directly, with
//! one `#[cfg]`-gated implementation per platform behind a shared signature.

#[cfg(target_os = "macos")]
pub mod macos;

use std::sync::OnceLock;

/// Conservative assumption when the OS won't say how much RAM it has. Sized so
/// the RAM-proportional budgets land at their floors rather than somewhere a
/// small machine can't afford.
const RAM_FALLBACK_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Total physical RAM in bytes, queried once and cached. Used to size
/// RAM-proportional cache budgets (`previews`, `navigation::preloader`) so a
/// small machine stays frugal and a big one gets headroom. Falls back to
/// [`RAM_FALLBACK_BYTES`] if the query fails.
pub fn total_physical_ram_bytes() -> u64 {
    static RAM: OnceLock<u64> = OnceLock::new();
    *RAM.get_or_init(|| {
        query_total_physical_ram_bytes().unwrap_or_else(|| {
            log::warn!("Couldn't read total physical RAM; assuming {RAM_FALLBACK_BYTES} bytes");
            RAM_FALLBACK_BYTES
        })
    })
}

/// macOS: `hw.memsize` is the total physical memory in bytes.
#[cfg(target_os = "macos")]
fn query_total_physical_ram_bytes() -> Option<u64> {
    let mut value: u64 = 0;
    let mut size = size_of::<u64>();
    // SAFETY: `sysctlbyname` writes a u64 into our u64-sized buffer; `size`
    // tracks the buffer length and is updated in place. No aliasing.
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            &mut value as *mut u64 as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && value > 0).then_some(value)
}

/// Linux: `/proc/meminfo`'s `MemTotal` line, in KB.
#[cfg(target_os = "linux")]
fn query_total_physical_ram_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_total_bytes(&meminfo)
}

/// Pull `MemTotal` out of `/proc/meminfo` and convert it to bytes. The line
/// reads `MemTotal:       16077216 kB`; the unit is always KB in practice, but
/// we honour whatever it says rather than assuming. Split out so the parse is
/// testable on any host.
#[cfg(any(target_os = "linux", test))]
fn parse_meminfo_total_bytes(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|line| line.starts_with("MemTotal:"))?;
    let mut fields = line.split_whitespace().skip(1);
    let value: u64 = fields.next()?.parse().ok()?;
    let multiplier = match fields.next() {
        Some("kB") | Some("KB") | None => 1024,
        Some("mB") | Some("MB") => 1024 * 1024,
        Some(_) => 1,
    };
    value.checked_mul(multiplier).filter(|bytes| *bytes > 0)
}

/// Windows: `GlobalMemoryStatusEx` reports installed physical memory in bytes.
#[cfg(target_os = "windows")]
fn query_total_physical_ram_bytes() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: u32::try_from(size_of::<MEMORYSTATUSEX>()).ok()?,
        ..Default::default()
    };
    // SAFETY: `GlobalMemoryStatusEx` fills our stack struct, whose size we
    // declare in `dwLength` exactly as the API requires.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    (status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}

/// Any other platform: no reading available yet, so callers get the fallback.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn query_total_physical_ram_bytes() -> Option<u64> {
    None
}

/// A fixed environment table, for testing code that resolves per-platform
/// paths out of environment variables. Lets a test assert about every
/// platform's layout from whichever host runs it, without mutating the process
/// environment (which is `unsafe` and races other tests).
#[cfg(test)]
pub(crate) fn fixed_env(
    pairs: &[(&str, &str)],
) -> impl Fn(&str) -> Option<std::ffi::OsString> + use<> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| std::ffi::OsString::from(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_env_answers_only_what_it_was_given() {
        let env = fixed_env(&[("HOME", "/home/dave")]);
        assert_eq!(env("HOME").as_deref(), Some("/home/dave".as_ref()));
        assert_eq!(env("APPDATA"), None);
    }

    #[test]
    fn total_physical_ram_is_plausible() {
        let ram = total_physical_ram_bytes();
        assert!(ram >= 1024 * 1024 * 1024, "at least 1 GB, got {ram} bytes");
        assert!(
            ram <= 8 * 1024 * 1024 * 1024 * 1024,
            "at most 8 TB, got {ram} bytes"
        );
    }

    #[test]
    fn meminfo_total_parses_kb() {
        let meminfo = "MemTotal:       16077216 kB\nMemFree:         1234567 kB\n";
        assert_eq!(parse_meminfo_total_bytes(meminfo), Some(16_077_216 * 1024));
    }

    /// `MemTotal` isn't always the first line, and a `MemTotalFoo` lookalike
    /// must not match.
    #[test]
    fn meminfo_total_finds_its_own_line() {
        let meminfo = "MemAvailable:   9000 kB\nMemTotal:       2048 kB\n";
        assert_eq!(parse_meminfo_total_bytes(meminfo), Some(2048 * 1024));
    }

    #[test]
    fn meminfo_without_total_is_none() {
        assert_eq!(parse_meminfo_total_bytes("MemFree: 1234 kB\n"), None);
        assert_eq!(parse_meminfo_total_bytes(""), None);
        assert_eq!(parse_meminfo_total_bytes("MemTotal:       0 kB\n"), None);
        assert_eq!(parse_meminfo_total_bytes("MemTotal:\n"), None);
    }
}
