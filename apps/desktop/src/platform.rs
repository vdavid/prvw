//! Platform-specific integrations.
//!
//! Per-platform submodules live under `platform::<os>` and are gated with `#[cfg]`.
//! When a second platform lands, mirror the `macos/` shape with its own submodule.
//! Small per-OS queries that don't warrant a submodule live here directly, with
//! one `#[cfg]`-gated implementation per platform behind a shared signature.

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use std::sync::OnceLock;
use std::time::Duration;

/// Conservative assumption when the OS won't say how much RAM it has. Sized so
/// the RAM-proportional budgets land at their floors rather than somewhere a
/// small machine can't afford.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // see `total_physical_ram_bytes`
const RAM_FALLBACK_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Total physical RAM in bytes, queried once and cached. Sizes `previews`'
/// RAM-proportional cache budget, so a small machine stays frugal and a big one
/// gets headroom. Falls back to [`RAM_FALLBACK_BYTES`] if the query fails.
///
/// `navigation::preloader` deliberately does **not** use this: its window is
/// capped and it drops everything outside that window on every navigation, so a
/// bigger budget has nothing to buy (`docs/notes/preload-window-and-cache-budget.md`).
/// That leaves `previews`, which is macOS-only until M3 of
/// `docs/specs/cross-platform-plan.md` gives it a Windows tier — hence the
/// `dead_code` allowance rather than a `cfg`. The per-OS queries below are
/// written and tested now precisely so M3 finds them ready.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
///
/// A unit we don't recognise gives up rather than guessing, so the caller falls
/// back to [`RAM_FALLBACK_BYTES`] and logs. Treating an unknown suffix as bytes
/// would silently report a 16 GB machine as having 16 MB, which reads as a
/// plausible number and quietly pins every RAM-proportional budget to its floor.
#[cfg(any(target_os = "linux", test))]
fn parse_meminfo_total_bytes(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|line| line.starts_with("MemTotal:"))?;
    let mut fields = line.split_whitespace().skip(1);
    let value: u64 = fields.next()?.parse().ok()?;
    let multiplier = match fields.next() {
        Some("kB") | Some("KB") | None => 1024,
        Some("mB") | Some("MB") => 1024 * 1024,
        Some(_) => return None,
    };
    value.checked_mul(multiplier).filter(|bytes| *bytes > 0)
}

/// Windows: `GlobalMemoryStatusEx` reports installed physical memory in bytes.
#[cfg(target_os = "windows")]
fn query_total_physical_ram_bytes() -> Option<u64> {
    // Leading `::` because this module has a `windows` submodule of its own now.
    use ::windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

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

/// What the OS calls "not a double-click any more", when it won't say.
///
/// macOS and Windows both ship 500 ms as their own default, and the app's own long-standing value
/// was 400 ms; the middle of that is not worth inventing, so this is Windows' documented default
/// and only Linux ever sees it.
const DOUBLE_CLICK_FALLBACK: Duration = Duration::from_millis(500);

/// How long after a click a second one still counts as a double-click.
///
/// It's a system-wide accessibility setting on both platforms that have one, and someone who
/// slowed it down did so because a fixed 400 ms was too fast for them. Asked per click rather than
/// cached: clicks happen at human speed, and the setting can change while the app runs.
pub fn double_click_interval() -> Duration {
    query_double_click_interval().unwrap_or(DOUBLE_CLICK_FALLBACK)
}

/// macOS: `NSEvent.doubleClickInterval`, in seconds.
#[cfg(target_os = "macos")]
fn query_double_click_interval() -> Option<Duration> {
    let seconds = objc2_app_kit::NSEvent::doubleClickInterval();
    (seconds > 0.0).then(|| Duration::from_secs_f64(seconds))
}

/// Windows: `GetDoubleClickTime`, in milliseconds.
#[cfg(target_os = "windows")]
fn query_double_click_interval() -> Option<Duration> {
    // SAFETY: no arguments, no out-parameters, and it can't fail.
    let millis = unsafe { ::windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
    (millis > 0).then(|| Duration::from_millis(u64::from(millis)))
}

/// Any other platform: no desktop-wide setting to read (X11 and Wayland leave it to each toolkit),
/// so callers get the fallback.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn query_double_click_interval() -> Option<Duration> {
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

    /// Whatever this host says, it has to be a usable answer: a zero interval would make every
    /// double-click impossible and a huge one would turn two separate clicks into one.
    #[test]
    fn the_double_click_interval_is_usable() {
        let interval = double_click_interval();
        assert!(
            interval >= Duration::from_millis(100) && interval <= Duration::from_secs(5),
            "got {interval:?}"
        );
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

    /// Giving up beats guessing: reading an unknown suffix as bytes would call
    /// a 16 GB machine a 16 MB one, which is plausible enough to go unnoticed.
    #[test]
    fn meminfo_with_an_unknown_unit_is_none() {
        assert_eq!(parse_meminfo_total_bytes("MemTotal:  16077216 gB\n"), None);
        assert_eq!(parse_meminfo_total_bytes("MemTotal:  16077216 ?\n"), None);
    }
}
