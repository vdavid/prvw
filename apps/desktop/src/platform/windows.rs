//! Windows-specific glue. Mirrors `platform::macos`: submodules live in the `windows/` directory
//! beside this file, and the small queries that don't warrant one stay here.

pub mod clipboard;
/// Dark chrome for the Win32 windows, which Windows still has no public API for.
pub mod dark_mode;
pub mod msg_hook;
pub mod print;
pub mod ui_common;
/// Debug-only window photograph, served by the QA server's `screenshot_window` tool.
#[cfg(debug_assertions)]
pub mod window_capture;

use windows::Win32::Foundation::{GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    SetStdHandle,
};
use windows::core::w;

/// Make sure this process's stderr goes somewhere a person can read, and report whether ANSI
/// escapes will render there.
///
/// `None` means there's nowhere to write at all, which is the normal case for a GUI-subsystem
/// binary launched from Explorer, the Start menu, or a taskbar pin. `crate::logging` falls back to
/// a file then.
///
/// A handle we already have is never replaced: `cargo run`, a shell redirect, and the E2E
/// harness's pipe all arrive that way, and attaching a console over them would throw their output
/// away.
pub fn connect_stderr() -> Option<bool> {
    if let Some(handle) = existing_handle(STD_ERROR_HANDLE) {
        return Some(enable_ansi(handle));
    }

    // SAFETY: no arguments to get wrong. It fails when there's no parent console, which is the
    // case we're testing for.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err() {
        return None;
    }

    // Attaching gives the process a console but leaves its standard handles unset, so open the
    // console's own output device and point stdout and stderr at it. Rust's `println!` and
    // `eprintln!` re-read these handles on every write, so this is all it takes.
    // SAFETY: a null security descriptor and no template handle are both valid here, and the
    // returned handle is checked before use.
    let console = unsafe {
        CreateFileW(
            w!("CONOUT$"),
            GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };
    let console = match console {
        Ok(handle) if !handle.is_invalid() => handle,
        _ => return None,
    };

    // SAFETY: `console` is a valid handle we just opened.
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, console);
        let _ = SetStdHandle(STD_ERROR_HANDLE, console);
    }
    Some(enable_ansi(console))
}

/// The handle already sitting in one of the standard slots, if the parent gave us a usable one.
fn existing_handle(slot: windows::Win32::System::Console::STD_HANDLE) -> Option<HANDLE> {
    // SAFETY: reads one of three constant slots and returns a handle we don't own.
    // `is_invalid` covers both answers we can get for "nothing here": a null handle and
    // INVALID_HANDLE_VALUE.
    let handle = unsafe { GetStdHandle(slot) }.ok()?;
    (!handle.is_invalid()).then_some(handle)
}

/// Turn on virtual-terminal processing so the log formatter's color escapes render as colors.
///
/// Returns false when the handle isn't a console at all (a pipe or a redirect to a file), where
/// escape sequences would just be noise in the captured output.
fn enable_ansi(handle: HANDLE) -> bool {
    let mut mode = CONSOLE_MODE::default();
    // SAFETY: `handle` is valid; `mode` is a plain out-parameter.
    if unsafe { GetConsoleMode(handle, &mut mode) }.is_err() {
        return false;
    }
    if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != CONSOLE_MODE(0) {
        return true;
    }
    // SAFETY: same handle, and the mode only adds a documented flag.
    unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) }.is_ok()
}

/// The face name of the font Windows uses for its own UI text, from `lfMessageFont`.
///
/// This is what the Windows UI design calls for: reading the metric gives "Segoe UI" on both
/// Windows 10 and 11, so the overlay matches the desktop without a version branch. `None` means
/// the query failed, and `render::text` falls back to its own list.
///
/// Asked once and kept, since a font system can be built more than once per process.
pub fn system_ui_font_name() -> Option<&'static str> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(query_ui_font_name).as_deref()
}

fn query_ui_font_name() -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        SystemParametersInfoW,
    };

    let mut metrics = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `cbSize` tells Windows how big the buffer is, and the pointer is to that same
    // struct. The DPI-scaled sizes it also fills in don't matter here; only the name does.
    unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            Some(std::ptr::from_mut(&mut metrics).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;

    let name = metrics.lfMessageFont.lfFaceName;
    let end = name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(name.len());
    String::from_utf16(&name[..end])
        .ok()
        .filter(|name| !name.is_empty())
}

/// How many lines of content one wheel notch moves, from `SPI_GETWHEELSCROLLLINES`.
///
/// Three by default, and the only place Windows lets someone say their wheel is too slow, so
/// `crate::scroll` scales the zoom rate by it. `WHEEL_PAGESCROLL` ("one screen at a time")
/// arrives as `u32::MAX` and is passed along as-is; the caller clamps.
///
/// Asked once and kept: a query per scroll event would be wasted work, and the setting changing
/// mid-session is rare enough to let the next launch pick it up.
pub fn wheel_scroll_lines() -> u32 {
    static LINES: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *LINES.get_or_init(|| query_wheel_scroll_lines().unwrap_or(3))
}

fn query_wheel_scroll_lines() -> Option<u32> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };

    let mut lines: u32 = 0;
    // SAFETY: `SPI_GETWHEELSCROLLLINES` writes one `UINT` through `pvParam`, which is what
    // `lines` is. `uiParam` is unused for this action.
    unsafe {
        SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some(std::ptr::from_mut(&mut lines).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    (lines > 0).then_some(lines)
}

/// The work area of the monitor `window` is on, in physical virtual-desktop pixels.
///
/// `rcWork` is `rcMonitor` minus the taskbar and any other appbar, so a window sized and centered
/// against it can't come up tucked underneath them. `MONITOR_DEFAULTTONEAREST` answers for a
/// window straddling two monitors the same way Windows itself decides which monitor's DPI a
/// window takes, so this and `Window::scale_factor` always name the same display.
///
/// `None` when the window has no Win32 handle yet or the query fails; the caller falls back to
/// winit's full monitor rect.
pub fn monitor_work_area(window: &winit::window::Window) -> Option<crate::window::PhysicalRect> {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut std::ffi::c_void);

    // SAFETY: `hwnd` comes from winit's live window. `MonitorFromWindow` can't fail with
    // `DEFAULTTONEAREST`, but a null answer is still checked below.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return None;
    }

    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `cbSize` declares the buffer size, as the API requires, and the pointer is to that
    // same struct.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }

    let work = info.rcWork;
    let width = f64::from(work.right - work.left);
    let height = f64::from(work.bottom - work.top);
    (width > 0.0 && height > 0.0).then(|| crate::window::PhysicalRect {
        x: f64::from(work.left),
        y: f64::from(work.top),
        width,
        height,
    })
}
