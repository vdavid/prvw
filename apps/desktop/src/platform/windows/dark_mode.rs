//! Dark chrome for Prvw's Win32 windows, and the honest limits of it.
//!
//! ## Why this is hand-rolled
//!
//! There is still no supported public dark-mode API for Win32 common controls
//! (`docs/specs/windows-ui-design.md` collects the sources). Every app that does it reaches for
//! the same three undocumented `uxtheme.dll` exports, by ordinal, and so do we:
//!
//! - **135**, `SetPreferredAppMode`, once per process, to let comctl32 render dark at all.
//! - **133**, `AllowDarkModeForWindow`, per window.
//! - `SetWindowTheme(hwnd, "DarkMode_Explorer", null)`, per control, which is documented.
//!
//! Undocumented means it can stop working, so every call here is best-effort: a missing export
//! or a failed call leaves the window light rather than half-painted, and nothing above has to
//! handle an error.
//!
//! ## What decides the theme, and what decides the colours
//!
//! Neither is here. `crate::chrome` owns both, because both are pure and this file can't be run
//! from a Mac: [`crate::chrome::theme_for`] is the three-input decision about light versus dark,
//! and [`crate::chrome::Theme::background`] and `text` are the colour table. This module reads
//! the three inputs out of the system, turns a [`Color`] into a `COLORREF`, keeps the brushes,
//! and answers `WM_CTLCOLOR*`.

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, COLOR_BTNTEXT, COLOR_GRAYTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateSolidBrush,
    GetSysColor, GetSysColorBrush, HBRUSH, HDC, OPAQUE, SYS_COLOR_INDEX, SetBkColor, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW,
};
use windows::core::{BOOL, PCSTR, PCWSTR, w};

use crate::chrome::{self, Color, FIRST_DARK_MODE_BUILD, Ink, Surface, SystemColor, Theme};

/// `PreferredAppMode::AllowDark`: follow the system, and let a window opt in per window.
const ALLOW_DARK: i32 = 1;

/// The theme name `SetWindowTheme` takes for a control. `DarkMode_Explorer` is reported to get
/// about 95% of the way, with poor highlight contrast on Windows 11; the alternatives are
/// per-control-class (`DarkMode_CFD` for combos, `DarkMode_ItemsView::ListView`) and this box
/// has neither.
fn control_theme(theme: Theme) -> PCWSTR {
    match theme {
        Theme::Light => w!("Explorer"),
        Theme::Dark => w!("DarkMode_Explorer"),
    }
}

/// What this machine is set to right now. Read per window open, so a person who flips the
/// system theme and reopens the box gets the new one.
pub fn current_theme() -> Theme {
    chrome::theme_for(os_build(), apps_use_light_theme(), high_contrast_on())
}

/// Let comctl32 render dark in this process. Idempotent, and the first thing any dark window
/// has to do: `AllowDarkModeForWindow` does nothing without it.
pub fn allow_dark_mode_for_app() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        if os_build() < FIRST_DARK_MODE_BUILD {
            return;
        }
        let Some(set_preferred_app_mode) = uxtheme_export(135) else {
            return;
        };
        // SAFETY: ordinal 135 on build 18362 and later is
        // `PreferredAppMode WINAPI SetPreferredAppMode(PreferredAppMode)`, a plain enum in and
        // out. The build gate above is what keeps 17763's different signature off this call.
        let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 =
            unsafe { std::mem::transmute(set_preferred_app_mode) };
        // SAFETY: as above; the callee takes no pointers.
        unsafe { set_preferred_app_mode(ALLOW_DARK) };
        log::debug!("Dark mode allowed for the process");
    });
}

/// Dress one window and its controls. Call on the window first, then on each control.
pub fn apply_to_window(hwnd: HWND, theme: Theme) {
    if let Some(allow_dark_mode_for_window) = uxtheme_export(133) {
        // SAFETY: ordinal 133 is `BOOL WINAPI AllowDarkModeForWindow(HWND, BOOL)` on every
        // build at or above our floor. `hwnd` is a live window this thread owns.
        let allow_dark_mode_for_window: unsafe extern "system" fn(HWND, BOOL) -> BOOL =
            unsafe { std::mem::transmute(allow_dark_mode_for_window) };
        // SAFETY: as above.
        let _ = unsafe { allow_dark_mode_for_window(hwnd, BOOL::from(theme == Theme::Dark)) };
    }
    // SAFETY: documented API. A null third argument means "no sub-app-name override".
    let _ = unsafe { SetWindowTheme(hwnd, control_theme(theme), None) };
}

/// Dress a window and every control under it.
///
/// [`apply_to_window`] is per window, and that's enough while a window is being built, because
/// every control is dressed as it's created. A live theme switch is the case that needs this
/// one: the controls already exist, and a dialog's pages sit between them and the frame.
pub fn apply_to_tree(hwnd: HWND, theme: Theme) {
    apply_to_window(hwnd, theme);
    // SAFETY: a live window of ours, and a callback that only reads back the flag this call
    // put in the `LPARAM`.
    unsafe {
        let _ = EnumChildWindows(
            Some(hwnd),
            Some(dress_child),
            LPARAM(matches!(theme, Theme::Dark) as isize),
        );
    }
}

/// `EnumChildWindows`' callback: one control, dressed the way the `LPARAM` says.
unsafe extern "system" fn dress_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let theme = if lparam.0 == 0 {
        Theme::Light
    } else {
        Theme::Dark
    };
    apply_to_window(hwnd, theme);
    true.into()
}

/// One export of `uxtheme.dll`, by ordinal. `None` when the DLL or the ordinal isn't there,
/// which is the whole error story: the caller stays light.
fn uxtheme_export(ordinal: u16) -> Option<unsafe extern "system" fn() -> isize> {
    static UXTHEME: OnceLock<Option<isize>> = OnceLock::new();
    let module = (*UXTHEME.get_or_init(|| {
        // SAFETY: a constant name, and `SEARCH_SYSTEM32` is what keeps a `uxtheme.dll` planted
        // next to the executable from being loaded instead of Windows' own.
        unsafe { LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
            .ok()
            .map(|module| module.0 as isize)
    }))?;
    let module = windows::Win32::Foundation::HMODULE(module as *mut c_void);
    // SAFETY: `MAKEINTRESOURCEA` is an ordinal in the low word of an otherwise null pointer,
    // which is how `GetProcAddress` distinguishes an ordinal from a name.
    unsafe { GetProcAddress(module, PCSTR(ordinal as usize as *const u8)) }
}

/// The Windows build number, from the registry.
///
/// `CurrentBuildNumber` is a `REG_SZ` and has been since Windows NT. Reading it beats
/// `GetVersionExW`, which is deprecated and answers from the manifest rather than from the
/// running OS. `0` when it can't be read, which reads as "too old for dark mode".
fn os_build() -> u32 {
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| {
        registry_string(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            w!("CurrentBuildNumber"),
        )
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
    })
}

/// `HKCU\…\Themes\Personalize\AppsUseLightTheme`. `None` when it isn't set.
///
/// It's the value PowerToys, WPF, and WinForms all read. `ShouldAppsUseDarkMode` (ordinal 132)
/// is the alternative and it has reports of answering `true` unconditionally on Windows 11
/// 23H2, so the registry is the more reliable source.
fn apps_use_light_theme() -> Option<u32> {
    registry_dword(
        HKEY_CURRENT_USER,
        w!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
        w!("AppsUseLightTheme"),
    )
}

fn high_contrast_on() -> bool {
    let mut contrast = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: `cbSize` declares the buffer, and the pointer is to that same struct.
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            Some(std::ptr::from_mut(&mut contrast).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    read.is_ok() && contrast.dwFlags.contains(HCF_HIGHCONTRASTON)
}

fn registry_dword(
    root: windows::Win32::System::Registry::HKEY,
    path: PCWSTR,
    name: PCWSTR,
) -> Option<u32> {
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `RRF_RT_REG_DWORD` makes the call refuse anything that isn't a 4-byte DWORD, and
    // `size` both declares and receives the buffer length.
    let status = unsafe {
        RegGetValueW(
            root,
            path,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut value).cast()),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(value)
}

fn registry_string(
    root: windows::Win32::System::Registry::HKEY,
    path: PCWSTR,
    name: PCWSTR,
) -> Option<String> {
    // Long enough for anything this module reads; the call fails rather than truncating.
    let mut buffer = [0u16; 64];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: `size` is the byte length of `buffer`, and `RRF_RT_REG_SZ` guarantees the bytes
    // written back are UTF-16 with a terminator inside that length.
    let status = unsafe {
        RegGetValueW(
            root,
            path,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(0);
    (end > 0).then(|| String::from_utf16_lossy(&buffer[..end]))
}

/// What `GetSysColor` and `GetSysColorBrush` call each of `chrome`'s colour jobs.
fn system_index(color: SystemColor) -> SYS_COLOR_INDEX {
    match color {
        SystemColor::ButtonFace => COLOR_BTNFACE,
        SystemColor::ButtonText => COLOR_BTNTEXT,
        SystemColor::Window => COLOR_WINDOW,
        SystemColor::WindowText => COLOR_WINDOWTEXT,
        SystemColor::GrayText => COLOR_GRAYTEXT,
    }
}

/// One of `chrome`'s colours as the `COLORREF` Win32 wants.
pub fn resolve(color: Color) -> COLORREF {
    match color {
        // SAFETY: a constant index, and the call has no failure mode.
        Color::System(system) => COLORREF(unsafe { GetSysColor(system_index(system)) }),
        Color::Fixed(value) => COLORREF(value),
    }
}

/// What a surface's background is filled with: `WM_ERASEBKGND` paints with it, and the
/// `WM_CTLCOLOR*` reply hands it back so a control's own background matches.
///
/// ❌ **Never delete one of these.** Light hands back a system brush, which Windows owns and
/// keeps current, so a high-contrast scheme or a custom colour arrives without us asking for it.
/// Dark has to be ours, and each is made once and never freed: the process outlives every window
/// painting with it.
pub fn background_brush(theme: Theme, surface: Surface) -> HBRUSH {
    if theme == Theme::Light {
        let Color::System(system) = theme.background(surface) else {
            // `the_light_theme_never_names_a_colour` is what makes this unreachable.
            return HBRUSH::default();
        };
        // SAFETY: a constant index. The brush belongs to the system and must not be deleted.
        return unsafe { GetSysColorBrush(system_index(system)) };
    }
    static DARK_DIALOG: OnceLock<isize> = OnceLock::new();
    static DARK_FIELD: OnceLock<isize> = OnceLock::new();
    let cell = match surface {
        Surface::Dialog => &DARK_DIALOG,
        Surface::Field => &DARK_FIELD,
    };
    let brush = *cell.get_or_init(|| {
        // SAFETY: a colour in, a brush out. A failed call answers with a null brush, which
        // Windows reads as "no brush" rather than misbehaving.
        let brush = unsafe { CreateSolidBrush(resolve(theme.background(surface))) };
        brush.0 as isize
    });
    HBRUSH(brush as *mut c_void)
}

/// Answer a `WM_CTLCOLOR*`: colour the device context Windows is about to draw the control with,
/// and hand back the brush it should fill the control's background with.
///
/// All six messages get the same reply, because the message is the wrong thing to key on: a
/// read-only edit sends `WM_CTLCOLORSTATIC` and arrives indistinguishable from a label. The
/// control's own class is what decides its surface (`chrome::surface_for_class`), and `control`
/// is the message's `lParam` — the control's window, or the dialog itself for
/// `WM_CTLCOLORDLG`.
///
/// The return is an `isize` rather than an `HBRUSH` because that's what a window procedure hands
/// back, and because a caller that forgot to return it would paint nothing.
pub fn paint_control(hdc: HDC, control: HWND, theme: Theme, ink: Ink) -> isize {
    let surface = chrome::surface_for_class(&class_name(control));
    // Text on the window is drawn transparently over a background the brush has already filled,
    // which is what keeps a label from stamping a rectangle of its own onto it. A field draws
    // its text opaquely instead, so a repainted run erases what was under it. ❌ Neither is
    // droppable as a no-op: `OPAQUE` is the default of a *fresh* device context, and Windows
    // hands the same one to the next control it asks about.
    let mode = match surface {
        Surface::Dialog => TRANSPARENT,
        Surface::Field => OPAQUE,
    };
    // SAFETY: `hdc` is the device context Windows passed with the message, valid for the
    // duration of it. None of the three calls can fail in a way worth handling.
    unsafe {
        SetTextColor(hdc, resolve(theme.text(surface, ink)));
        SetBkColor(hdc, resolve(theme.background(surface)));
        SetBkMode(hdc, mode);
    }
    background_brush(theme, surface).0 as isize
}

/// A window's class name. Empty for a handle Windows doesn't recognise, which
/// `chrome::surface_for_class` reads as the window's own surface.
fn class_name(hwnd: HWND) -> String {
    // Longer than every class Prvw creates or hosts; the call truncates rather than failing,
    // and a truncated name simply doesn't match, which lands on the safe surface.
    let mut buffer = [0u16; 64];
    // SAFETY: the length is the buffer's, and the call writes no more than that.
    let written = unsafe { GetClassNameW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..written.max(0) as usize])
}
