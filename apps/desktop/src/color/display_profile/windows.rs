//! The ICC profile Windows associates with the monitor a window is on.
//!
//! ## The path through Win32, and why it takes this route
//!
//! `MonitorFromWindow` names the monitor, `GetMonitorInfoW` gives that monitor's GDI device name
//! (`\\.\DISPLAY1`), `CreateDCW` opens a device context for **that** display, and `GetICMProfileW`
//! reads the profile file associated with it. Reading the profile then means reading a file: the
//! call hands back a path, not bytes.
//!
//! The obvious shortcut is `GetICMProfileW` on the window's own DC, and it's one monitor short of
//! right. A window straddling two displays has one DC, so that answer is whatever GDI decides the
//! window mostly belongs to, which need not be the monitor `MonitorFromWindow` names and which is
//! therefore not the monitor [`super::MonitorTracker`] is watching. Asking the named display
//! directly keeps the profile and the monitor identity talking about the same screen.
//!
//! ## What a machine with no calibration answers
//!
//! Most of them. Windows ships no per-display profile of its own, so unless the user calibrated
//! the monitor or the vendor's installer associated one, `GetICMProfileW` answers with the system
//! default, `sRGB Color Space Profile.icm`. That is the right answer, and `color::transform_icc`
//! makes it free: it probes the transform and skips one that moves nothing. So an uncalibrated
//! Windows machine pays nothing for this and a calibrated one gets the whole point of the app.
//! Don't read a quiet log line here as a failure.
//!
//! ## Not yet: the Windows 11 advanced-colour profile
//!
//! `ColorProfileGetDisplayDefault` returns the profile Windows 11 uses for a display in HDR mode,
//! which is a refinement over this for one case: SDR content shown while the display is in HDR
//! mode. It is deliberately absent, for two reasons worth writing down before someone adds it
//! without them.
//!
//! First, it can't be reached the way it looks like it can. The plan called for "a runtime version
//! check with a `GetICMProfileW` fallback", and a runtime check is too late: the `windows` crate
//! declares its imports with `raw-dylib`, so calling it at all puts
//! `mscms.dll!ColorProfileGetDisplayDefault` in the executable's import table, and Windows 10
//! (build 19045, below the 20348 the export arrived in) would fail to *load the process*. Prvw
//! supports Windows 10 22H2 at full fidelity, so reaching it means `LoadLibraryW` plus
//! `GetProcAddress` plus a hand-written signature, not a version check.
//!
//! Second, it needs the display's adapter LUID and source id, which come from `QueryDisplayConfig`
//! and a `DISPLAYCONFIG_SOURCE_DEVICE_NAME` lookup to match the GDI device name above. That is a
//! few hundred lines that can't be exercised anywhere in this project's test setup, for a case
//! where our own HDR path doesn't consult the display profile at all (it hands the compositor
//! extended-range values and lets Windows map them). Worth doing on a machine that can show it
//! working; not worth guessing at.

use std::ffi::{OsString, c_void};
use std::io::Read;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::UI::ColorSystem::GetICMProfileW;
use windows::core::{PCWSTR, PWSTR};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::MonitorId;

/// The largest profile file we'll load. A display profile is a few kilobytes of matrices and
/// curves, or a few hundred with a big LUT; anything past this is a path that stopped pointing at
/// a profile, and reading it would stall the launch for nothing.
const MAX_PROFILE_BYTES: u64 = 8 * 1024 * 1024;

/// Characters of profile path to ask for on the first try. `GetICMProfileW` reports what it
/// actually needs when this isn't enough, and [`read_icm_profile`] asks again with that.
const FIRST_TRY_CHARS: u32 = 260;

/// The longest path we'll believe a second `GetICMProfileW` attempt asking for. Windows' own
/// ceiling for a path is 32,767 characters, so a request past that is a failure being reported
/// through the length rather than a real answer.
const MAX_PATH_CHARS: u32 = 32_768;

/// The ICC bytes for the display `window` is on, or `None` when Windows names no profile, or names
/// one that isn't there.
pub fn display_icc(window: &Window) -> Option<Vec<u8>> {
    let monitor = current_monitor_handle(window)?;
    let device = device_name(monitor)?;
    let path = profile_path(&device)?;

    let mut file = std::fs::File::open(&path)
        .inspect_err(|why| {
            log::warn!(
                "Windows names {} as the display profile, but it can't be read ({why})",
                path.display()
            );
        })
        .ok()?;
    let size = file.metadata().ok()?.len();
    if size > MAX_PROFILE_BYTES {
        log::warn!(
            "Ignoring {}: {size} bytes is too large to be a display profile",
            path.display()
        );
        return None;
    }

    let mut bytes = Vec::with_capacity(size as usize);
    file.read_to_end(&mut bytes).ok()?;
    log::info!(
        "Display ICC profile: {} bytes from {}{}",
        bytes.len(),
        path.display(),
        super::describe_for_log(&bytes)
    );
    super::usable_profile(bytes)
}

/// Which monitor `window` is on, for [`super::MonitorTracker`]. `None` before the window has a
/// Win32 handle.
pub fn current_monitor(window: &Window) -> Option<MonitorId> {
    let monitor = current_monitor_handle(window)?;
    Some(MonitorId(monitor.0 as usize as u64))
}

fn current_monitor_handle(window: &Window) -> Option<HMONITOR> {
    let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut c_void);

    // SAFETY: `hwnd` comes from winit's live window. `DEFAULTTONEAREST` means a window dragged
    // fully off every screen still names one, so there's no "between monitors" gap to handle.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    (!monitor.is_invalid()).then_some(monitor)
}

/// The GDI device name of a monitor (`\\.\DISPLAY1`), NUL-terminated so it can be handed straight
/// to `CreateDCW`.
fn device_name(monitor: HMONITOR) -> Option<Vec<u16>> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `monitor` is live, and the `MONITORINFOEXW` we point at declares its own larger size
    // through `cbSize`, which is how Win32 is told to fill in `szDevice` as well.
    let filled = unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo as *mut MONITORINFO) };
    if !filled.as_bool() {
        log::warn!("GetMonitorInfoW couldn't name the monitor, so there's no profile to read");
        return None;
    }

    let name = super::until_nul(&info.szDevice);
    if name.is_empty() {
        return None;
    }
    let mut terminated = name.to_vec();
    terminated.push(0);
    Some(terminated)
}

/// The path Windows has associated with one display, through a device context opened for exactly
/// that display.
fn profile_path(device: &[u16]) -> Option<PathBuf> {
    // SAFETY: `device` is a NUL-terminated GDI device name from `GetMonitorInfoW`. A null driver
    // and port are what a display DC takes, and the returned handle is released below on every
    // path out.
    let dc = unsafe { CreateDCW(None, PCWSTR(device.as_ptr()), None, None) };
    if dc.is_invalid() {
        log::warn!("Couldn't open a device context for the display, so its profile stays unread");
        return None;
    }
    let path = read_icm_profile(dc);
    // SAFETY: `dc` came from `CreateDCW` and nothing else holds it. A failure here would mean
    // the handle was already gone, which changes nothing we're about to do.
    let _ = unsafe { DeleteDC(dc) };
    path
}

/// `GetICMProfileW` against an open display DC. Up to two calls: the first learns the length, and
/// the second reads it, because a profile living under a long user path can outgrow the classic
/// `MAX_PATH` buffer every example of this call uses.
fn read_icm_profile(dc: HDC) -> Option<PathBuf> {
    let mut chars = FIRST_TRY_CHARS;
    let Some(buffer) = fill_profile_path(dc, &mut chars) else {
        // The one recoverable failure: the buffer was short, and `chars` now says how short.
        if chars <= FIRST_TRY_CHARS || chars > MAX_PATH_CHARS {
            log::warn!("Windows associates no ICC profile with this display");
            return None;
        }
        return fill_profile_path(dc, &mut chars.clone()).and_then(path_from_buffer);
    };
    path_from_buffer(buffer)
}

/// One `GetICMProfileW` call with a buffer of `*chars` wide characters. `None` when it fails,
/// leaving `*chars` holding whatever the call put there (the length it wanted, when that's why).
fn fill_profile_path(dc: HDC, chars: &mut u32) -> Option<Vec<u16>> {
    let mut buffer = vec![0u16; *chars as usize];
    // SAFETY: `chars` says how many `u16`s `buffer` holds and the call writes no more than that,
    // NUL-terminating on success. On failure it overwrites `chars` with the length it wants.
    let read = unsafe { GetICMProfileW(dc, chars, Some(PWSTR(buffer.as_mut_ptr()))) };
    read.as_bool().then_some(buffer)
}

fn path_from_buffer(buffer: Vec<u16>) -> Option<PathBuf> {
    let name = super::until_nul(&buffer);
    (!name.is_empty()).then(|| PathBuf::from(OsString::from_wide(name)))
}
