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
//! ## What decides the theme
//!
//! [`theme_for`] is the whole decision, and it's pure so it can be asserted rather than
//! eyeballed. Three inputs, in priority order:
//!
//! 1. **High contrast wins outright.** It's an accessibility setting, and overriding a person's
//!    contrast choice with our idea of a nice dark grey is exactly the wrong move.
//! 2. **The OS build**, because the ordinals only mean what we think they mean from 18362 on.
//!    Below that we stay light. ❌ Don't copy `win32-darkmode`'s exact-match build allowlist:
//!    it refuses 19045, which is Prvw's actual floor.
//! 3. **`AppsUseLightTheme`**, the value PowerToys, WPF, and WinForms all read.
//!    `ShouldAppsUseDarkMode` (ordinal 132) is the alternative and it has reports of answering
//!    `true` unconditionally on Windows 11 23H2, so the registry is the more reliable source.

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};
use windows::core::{BOOL, PCSTR, PCWSTR, w};

/// The first build where ordinal 135 is `SetPreferredAppMode(PreferredAppMode)` rather than
/// 17763's `AllowDarkModeForApp(BOOL)`. Below it, we don't try.
const FIRST_DARK_MODE_BUILD: u32 = 18362;

/// `PreferredAppMode::AllowDark`: follow the system, and let a window opt in per window.
const ALLOW_DARK: i32 = 1;

/// Which way a window paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    /// The theme name `SetWindowTheme` takes for a control. `DarkMode_Explorer` is reported to
    /// get about 95% of the way, with poor highlight contrast on Windows 11; the alternatives
    /// are per-control-class (`DarkMode_CFD` for combos, `DarkMode_ItemsView::ListView`) and
    /// this box has neither.
    fn control_theme(self) -> PCWSTR {
        match self {
            Theme::Light => w!("Explorer"),
            Theme::Dark => w!("DarkMode_Explorer"),
        }
    }

    /// Window background and body text, as `COLORREF` (`0x00BBGGRR`).
    ///
    /// The dark pair is Windows 11's own dialog grey and near-white, rather than pure black on
    /// pure white: comctl32's dark assets are drawn against that grey, and a black window behind
    /// them reads as two different dark themes touching.
    pub fn colors(self) -> (COLORREF, COLORREF) {
        match self {
            Theme::Light => (COLORREF(0x00FF_FFFF), COLORREF(0x0000_0000)),
            Theme::Dark => (COLORREF(0x0020_2020), COLORREF(0x00F0_F0F0)),
        }
    }
}

/// Which theme a window should paint in, given the three things the system can tell us.
///
/// `apps_use_light_theme` is `None` when the value isn't there at all, which is how a fresh
/// profile looks and means light.
pub fn theme_for(build: u32, apps_use_light_theme: Option<u32>, high_contrast: bool) -> Theme {
    if high_contrast || build < FIRST_DARK_MODE_BUILD {
        return Theme::Light;
    }
    match apps_use_light_theme {
        Some(0) => Theme::Dark,
        _ => Theme::Light,
    }
}

/// What this machine is set to right now. Read per window open, so a person who flips the
/// system theme and reopens the box gets the new one.
pub fn current_theme() -> Theme {
    theme_for(os_build(), apps_use_light_theme(), high_contrast_on())
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
    let _ = unsafe { SetWindowTheme(hwnd, theme.control_theme(), None) };
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

/// A solid brush for a theme's background, made once per theme and never freed: the About box
/// is the only client and the process outlives it.
pub fn background_brush(theme: Theme) -> windows::Win32::Graphics::Gdi::HBRUSH {
    static BRUSHES: OnceLock<[isize; 2]> = OnceLock::new();
    let brushes = BRUSHES.get_or_init(|| {
        [Theme::Light, Theme::Dark].map(|theme| {
            // SAFETY: a colour in, a brush out. A failed call returns a null brush, which the
            // window procedure passes to `DefWindowProc` and Windows treats as "no brush".
            let brush =
                unsafe { windows::Win32::Graphics::Gdi::CreateSolidBrush(theme.colors().0) };
            brush.0 as isize
        })
    });
    let index = usize::from(theme == Theme::Dark);
    windows::Win32::Graphics::Gdi::HBRUSH(brushes[index] as *mut c_void)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// High contrast is an accessibility setting and it beats everything, including an explicit
    /// dark preference on a build that could honour it.
    #[test]
    fn high_contrast_stays_light() {
        assert_eq!(theme_for(22631, Some(0), true), Theme::Light);
        assert_eq!(theme_for(19045, Some(0), true), Theme::Light);
    }

    /// 19045 is Prvw's floor and it gets dark mode. This is the case `win32-darkmode`'s
    /// exact-match build list gets wrong, and the reason we don't copy it.
    #[test]
    fn the_support_floor_gets_dark_mode() {
        assert_eq!(theme_for(19045, Some(0), false), Theme::Dark);
        assert_eq!(theme_for(19045, Some(1), false), Theme::Light);
    }

    /// Below 18362 the ordinal means something else, so we don't touch it.
    #[test]
    fn older_builds_stay_light_whatever_the_preference() {
        assert_eq!(theme_for(17763, Some(0), false), Theme::Light);
        assert_eq!(theme_for(0, Some(0), false), Theme::Light);
    }

    /// A profile that never set the value is a light profile.
    #[test]
    fn a_missing_preference_means_light() {
        assert_eq!(theme_for(22631, None, false), Theme::Light);
    }
}
