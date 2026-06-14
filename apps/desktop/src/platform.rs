//! Platform-specific integrations. Today, macOS only.
//!
//! Per-platform submodules live under `platform::<os>` and are gated with `#[cfg]`.
//! When a second platform lands, mirror the `macos/` shape with its own submodule.

#[cfg(target_os = "macos")]
pub mod macos;

use std::sync::OnceLock;

/// Total physical RAM in bytes, queried once and cached. Used to size
/// RAM-proportional cache budgets (see `thumbnails`) so a small machine
/// stays frugal and a big one gets headroom. Falls back to a conservative
/// 8 GB assumption if the query fails, keeping budgets sane on an
/// unexpected platform or error.
pub fn total_physical_ram_bytes() -> u64 {
    static RAM: OnceLock<u64> = OnceLock::new();
    *RAM.get_or_init(query_total_physical_ram_bytes)
}

const RAM_FALLBACK_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[cfg(target_os = "macos")]
fn query_total_physical_ram_bytes() -> u64 {
    // `hw.memsize` is the total physical memory in bytes.
    let mut value: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
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
    if rc == 0 && value > 0 {
        value
    } else {
        log::warn!("sysctl hw.memsize failed (rc={rc}); assuming {RAM_FALLBACK_BYTES} bytes");
        RAM_FALLBACK_BYTES
    }
}

#[cfg(not(target_os = "macos"))]
fn query_total_physical_ram_bytes() -> u64 {
    RAM_FALLBACK_BYTES
}
