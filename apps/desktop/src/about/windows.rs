//! # About Prvw, on Windows
//!
//! A small modeless popup under **Help → About Prvw**, which is where Windows keeps About.
//! [`crate::about::content`] says what's in it; this file is the Win32 half.
//!
//! ## Modeless, and why that isn't a detail
//!
//! `AGENTS.md` has the rule: **no nested message loop, ever.** A Win32 modal loop
//! (`DialogBoxParam`, `TaskDialogIndirect`) doesn't crash the way an AppKit modal does inside
//! winit's callbacks; it starves winit's pump, so `about_to_wait` stops running and the
//! slideshow timer freezes behind the box. So this is an ordinary owned popup, created and
//! returned from, and it hands its handle to [`crate::platform::windows::msg_hook`] so
//! `IsDialogMessageW` gives it Tab, Esc, Enter, and the arrow keys.
//!
//! It's built control by control rather than from a dialog template: the layout is nine
//! positions, and a template would put the copy in a resource script where the shared
//! `content` module can't reach it.
//!
//! ## Decision: not a task dialog
//!
//! `TaskDialogIndirect` would be four lines of code instead of this file, and it has two
//! problems. It blocks, so it would need a thread of its own and a second lifetime to reason
//! about. And **task dialogs don't follow dark mode**, so a light box would come out of a dark
//! app right next to the product's name. `docs/specs/windows-ui-design.md` reasons it out.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, FW_SEMIBOLD, FillRect, HDC, HFONT, InvalidateRect,
    SetBkColor, SetTextColor, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    ICC_LINK_CLASS, INITCOMMONCONTROLSEX, InitCommonControlsEx, NM_CLICK, NM_RETURN, NMHDR, NMLINK,
    WC_LINK,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use super::content::AboutContent;
use crate::platform::windows::dark_mode::{self, Theme};
use crate::platform::windows::msg_hook;

/// The Close button. `IDOK` rather than a private id so `IsDialogMessageW` fires it on Enter,
/// which it only does for the default push button.
const ID_CLOSE: i32 = IDOK.0;

/// One id per `SysLink`, so `WM_NOTIFY` can be handled without caring which one arrived: the
/// URL comes from the notification itself.
const ID_FIRST_LINK: i32 = 100;

/// The `SS_*` static styles, from `winuser.h`. The `windows` crate keeps them in
/// `Win32::System::SystemServices`, which is a large module to compile for three integers.
const SS_LEFT: u32 = 0x0000;
const SS_ICON: u32 = 0x0003;
/// Draw the icon at the size the control was given rather than the size the icon happens to be.
const SS_REALSIZECONTROL: u32 = 0x0040;

/// Layout, in the logical pixels of a 96-DPI screen. Everything is scaled by the window's own
/// DPI at build time, so the proportions hold at 125%, 150%, and 200%.
mod layout {
    pub const WIDTH: i32 = 400;
    pub const PADDING: i32 = 16;
    pub const ICON: i32 = 48;
    /// Between the icon and the heading beside it.
    pub const GAP: i32 = 12;
    pub const HEADING_HEIGHT: i32 = 28;
    pub const LINE: i32 = 18;
    /// Between one body line's top and the next one's.
    pub const LINE_STEP: i32 = 22;
    pub const BUTTON_WIDTH: i32 = 88;
    pub const BUTTON_HEIGHT: i32 = 26;
    /// The heading, next to the app icon.
    pub const HEADING_SCALE: f32 = 1.5;
}

/// The one About window, if it's open, and what it needs to repaint.
///
/// One at a time: a second About box is never what someone meant by clicking the menu twice.
struct AboutWindow {
    hwnd: HWND,
    theme: Theme,
    body_font: HFONT,
    heading_font: HFONT,
}

thread_local! {
    static OPEN: RefCell<Option<AboutWindow>> = const { RefCell::new(None) };
}

/// Show the About box, or bring the open one forward.
///
/// `parent` is the main window, which the box is owned by (so it stays in front of the app and
/// minimizes with it) and centered on.
pub fn show_window(parent: Option<&winit::window::Window>) {
    let parent_hwnd = parent.and_then(window_handle);

    let already_open = OPEN.with_borrow(|open| open.as_ref().map(|window| window.hwnd));
    if let Some(hwnd) = already_open {
        // SAFETY: a live window this thread owns. A refused activation is fine; the window is
        // already on screen either way.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }

    match build(parent_hwnd) {
        Some(hwnd) => {
            msg_hook::register_dialog(hwnd);
            log::debug!("About window shown");
        }
        None => log::warn!("Couldn't open the About window"),
    }
}

/// The winit window's `HWND`, or `None` before it exists.
fn window_handle(window: &winit::window::Window) -> Option<HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    Some(HWND(handle.hwnd.get() as *mut c_void))
}

fn build(parent: Option<HWND>) -> Option<HWND> {
    let content = AboutContent::host();
    dark_mode::allow_dark_mode_for_app();
    // `SysLink` is a v6 control. The manifest already asks for comctl32 v6; this registers the
    // class within it.
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LINK_CLASS,
    };
    // SAFETY: `dwSize` declares the struct, which is the whole contract.
    let _ = unsafe { InitCommonControlsEx(&controls) };

    let class = register_class()?;
    // SAFETY: no arguments to get wrong; the process module always exists.
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;

    let dpi = parent.map_or(USER_DEFAULT_SCREEN_DPI, |hwnd| {
        // SAFETY: a live window this thread owns.
        unsafe { GetDpiForWindow(hwnd) }
    });
    let scale = |value: i32| value * dpi as i32 / USER_DEFAULT_SCREEN_DPI as i32;

    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let client_width = scale(layout::WIDTH);
    let client_height = scale(client_height_at_96_dpi());
    let (window_width, window_height) = outer_size(client_width, client_height, style, dpi);
    let (x, y) = centered_on(parent, window_width, window_height);

    // SAFETY: the class is registered, the strings outlive the call, and an owner of `None` is
    // valid (it means an unowned popup, which only happens before the main window exists).
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            class,
            &HSTRING::from(content.window_title),
            style,
            x,
            y,
            window_width,
            window_height,
            parent,
            None,
            Some(instance.into()),
            None,
        )
    }
    .ok()?;

    let theme = dark_mode::current_theme();
    dark_mode::apply_to_window(hwnd, theme);
    set_caption_theme(hwnd, theme);

    let body_font = message_font(dpi, 1.0, false);
    let heading_font = message_font(dpi, layout::HEADING_SCALE, true);
    OPEN.with_borrow_mut(|open| {
        *open = Some(AboutWindow {
            hwnd,
            theme,
            body_font,
            heading_font,
        });
    });

    add_controls(hwnd, instance.into(), &content, dpi, theme);

    // SAFETY: a window we just created.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
    }
    Some(hwnd)
}

/// How tall the client area has to be, before DPI scaling. Derived from the same constants the
/// controls are placed with, so the two can't drift.
const fn client_height_at_96_dpi() -> i32 {
    let below_icon = layout::PADDING + layout::ICON + layout::LINE;
    // Tagline, author, the two-line licence, and the two links.
    let body = layout::LINE_STEP * 2 + (layout::LINE * 2 + 4) + layout::LINE_STEP * 2;
    below_icon + body + layout::BUTTON_HEIGHT + layout::PADDING
}

fn add_controls(hwnd: HWND, instance: HINSTANCE, content: &AboutContent, dpi: u32, theme: Theme) {
    let scale = |value: i32| value * dpi as i32 / USER_DEFAULT_SCREEN_DPI as i32;
    let content_width = layout::WIDTH - layout::PADDING * 2;
    let heading_left = layout::PADDING + layout::ICON + layout::GAP;
    let heading_width = layout::WIDTH - heading_left - layout::PADDING;

    let place = |class: PCWSTR, text: &str, style: WINDOW_STYLE, id: i32, rect: [i32; 4]| {
        // SAFETY: `hwnd` is the live parent, the class is one comctl32 registered, and the text
        // is copied by Windows before the call returns.
        let control = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                &HSTRING::from(text),
                style | WS_CHILD | WS_VISIBLE,
                scale(rect[0]),
                scale(rect[1]),
                scale(rect[2]),
                scale(rect[3]),
                Some(hwnd),
                Some(HMENU(id as usize as *mut c_void)),
                Some(instance),
                None,
            )
        };
        if let Ok(control) = control {
            dark_mode::apply_to_window(control, theme);
        }
        control.ok()
    };

    // The app icon, which `build.rs` puts in the executable as group icon 1.
    if let Some(icon) = place(
        w!("STATIC"),
        "",
        WINDOW_STYLE(SS_ICON | SS_REALSIZECONTROL),
        ID_FIRST_LINK - 1,
        [layout::PADDING, layout::PADDING, layout::ICON, layout::ICON],
    ) {
        set_app_icon(icon, instance, scale(layout::ICON));
    }

    let heading = place(
        w!("STATIC"),
        content.name,
        WINDOW_STYLE(SS_LEFT),
        ID_FIRST_LINK - 2,
        [
            heading_left,
            layout::PADDING,
            heading_width,
            layout::HEADING_HEIGHT,
        ],
    );

    let mut y = layout::PADDING + layout::HEADING_HEIGHT + 2;
    place(
        w!("STATIC"),
        content.version,
        WINDOW_STYLE(SS_LEFT),
        ID_FIRST_LINK - 3,
        [heading_left, y, heading_width, layout::LINE],
    );

    y = layout::PADDING + layout::ICON + layout::LINE;
    place(
        w!("STATIC"),
        content.tagline,
        WINDOW_STYLE(SS_LEFT),
        ID_FIRST_LINK - 4,
        [layout::PADDING, y, content_width, layout::LINE],
    );

    y += layout::LINE_STEP;
    place(
        w!("STATIC"),
        content.author,
        WINDOW_STYLE(SS_LEFT),
        ID_FIRST_LINK - 5,
        [layout::PADDING, y, content_width, layout::LINE],
    );

    // Two lines of room: at 400 logical pixels the licence sentence wraps once, and a `SysLink`
    // wraps at word boundaries like a static does.
    y += layout::LINE_STEP;
    place(
        WC_LINK,
        &content.license.markup(),
        WS_TABSTOP,
        ID_FIRST_LINK,
        [layout::PADDING, y, content_width, layout::LINE * 2 + 4],
    );

    y += layout::LINE * 2 + 4 + 4;
    place(
        WC_LINK,
        &link_markup(content.author_site.url, content.author_site.label),
        WS_TABSTOP,
        ID_FIRST_LINK + 1,
        [layout::PADDING, y, content_width, layout::LINE],
    );

    y += layout::LINE_STEP;
    place(
        WC_LINK,
        &link_markup(content.website.url, content.website.label),
        WS_TABSTOP,
        ID_FIRST_LINK + 2,
        [layout::PADDING, y, content_width, layout::LINE],
    );

    y += layout::LINE_STEP + 4;
    let close = place(
        w!("BUTTON"),
        "Close",
        WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
        ID_CLOSE,
        [
            layout::WIDTH - layout::PADDING - layout::BUTTON_WIDTH,
            y,
            layout::BUTTON_WIDTH,
            layout::BUTTON_HEIGHT,
        ],
    );

    apply_fonts(hwnd, heading);
    if let Some(close) = close {
        // SAFETY: a live control; the button takes focus so Enter and Space work at once.
        let _ = unsafe { SetFocus(Some(close)) };
    }
}

/// One link on its own line, as `SysLink` markup.
fn link_markup(url: &str, label: &str) -> String {
    format!("<a href=\"{url}\">{label}</a>")
}

/// Give every control the system UI font, and the heading its larger one. Windows hands a
/// hand-made control the ancient bitmap system font otherwise, which is the clearest sign a
/// dialog was built by hand.
fn apply_fonts(hwnd: HWND, heading: Option<HWND>) {
    let Some((body_font, heading_font)) = OPEN.with_borrow(|open| {
        open.as_ref()
            .map(|window| (window.body_font, window.heading_font))
    }) else {
        return;
    };

    let mut child = HWND::default();
    loop {
        // SAFETY: `GetWindow` walks this window's children and answers null at the end.
        let next = unsafe {
            GetWindow(
                if child.is_invalid() { hwnd } else { child },
                if child.is_invalid() {
                    GW_CHILD
                } else {
                    GW_HWNDNEXT
                },
            )
        };
        let Ok(next) = next else { break };
        if next.is_invalid() {
            break;
        }
        child = next;
        let font = if Some(child) == heading {
            heading_font
        } else {
            body_font
        };
        // SAFETY: a live control and a live font. `WM_SETFONT` copies nothing and takes no
        // ownership; the font outlives the window (see `WM_DESTROY`).
        unsafe {
            SendMessageW(
                child,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            )
        };
    }
}

/// The system UI font at this DPI, optionally larger and semibold for the heading.
///
/// `SystemParametersInfoForDpi` returns `lfMessageFont` already scaled for the DPI asked for,
/// which is why nothing here multiplies by a scale factor of its own.
fn message_font(dpi: u32, scale: f32, semibold: bool) -> HFONT {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `cbSize` declares the buffer and the pointer is to that same struct.
    let read = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            metrics.cbSize,
            Some(std::ptr::from_mut(&mut metrics).cast()),
            0,
            dpi,
        )
    };
    if read.is_err() {
        return HFONT::default();
    }

    let mut font = metrics.lfMessageFont;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a font height in logical units is small, and the product of two small numbers"
    )]
    if (scale - 1.0).abs() > f32::EPSILON {
        font.lfHeight = (font.lfHeight as f32 * scale) as i32;
    }
    if semibold {
        font.lfWeight = FW_SEMIBOLD.0 as i32;
    }
    // SAFETY: a fully initialized `LOGFONTW`. A failed call answers null, which `WM_SETFONT`
    // treats as "the default font".
    unsafe { CreateFontIndirectW(&font) }
}

/// Load group icon 1 out of our own executable at the size the static will draw it, so Windows
/// picks the right image from the icon rather than stretching the 256-pixel one down.
fn set_app_icon(control: HWND, instance: HINSTANCE, size: i32) {
    // SAFETY: `MAKEINTRESOURCEW(1)` is the group icon `build.rs` embeds; a miss answers with an
    // error and leaves the static empty.
    let icon = unsafe {
        LoadImageW(
            Some(instance),
            // `MAKEINTRESOURCEW(1)`: an ordinal in the low word of an otherwise null pointer.
            PCWSTR(std::ptr::without_provenance(1)),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
    };
    if let Ok(icon) = icon {
        // SAFETY: a live static and an icon handle Windows owns (loaded from a resource, so it
        // needs no destroy).
        unsafe {
            SendMessageW(
                control,
                STM_SETIMAGE,
                Some(WPARAM(IMAGE_ICON.0 as usize)),
                Some(LPARAM(icon.0 as isize)),
            )
        };
    }
}

/// Paint the caption bar to match. Without this a dark box wears a light title bar, which is
/// the first thing anyone notices.
fn set_caption_theme(hwnd: HWND, theme: Theme) {
    let dark = BOOL::from(theme == Theme::Dark);
    // SAFETY: the attribute takes a `BOOL`, and `size_of` says so. Unsupported on builds below
    // 18985, where it returns an error we ignore.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            std::ptr::from_ref::<BOOL>(&dark).cast(),
            size_of_val(&dark) as u32,
        )
    };
}

/// The outer window size that gives a client area of `width` by `height` at this DPI.
fn outer_size(width: i32, height: i32, style: WINDOW_STYLE, dpi: u32) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    // SAFETY: a plain in-out rect. A failure leaves it as the client rect, which is small by
    // the caption's height rather than wrong.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut rect, style, false, WS_EX_DLGMODALFRAME, dpi) };
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// Centered on the owner, or on the work area of the monitor it's on.
fn centered_on(parent: Option<HWND>, width: i32, height: i32) -> (i32, i32) {
    let mut rect = RECT::default();
    let known = parent.is_some_and(|hwnd| {
        // SAFETY: a live window this thread owns; the rect is a plain out-parameter.
        unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok()
    });
    if !known {
        return (CW_USEDEFAULT, CW_USEDEFAULT);
    }
    (
        rect.left + (rect.right - rect.left - width) / 2,
        rect.top + (rect.bottom - rect.top - height) / 2,
    )
}

/// Register the window class, once per process.
fn register_class() -> Option<PCWSTR> {
    static CLASS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let name = w!("PrvwAboutWindow");
    let registered = *CLASS.get_or_init(|| {
        // SAFETY: no arguments to get wrong; the process module always exists.
        let Ok(instance) = (unsafe { GetModuleHandleW(None) }) else {
            return false;
        };
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            // SAFETY: a system cursor, which needs no module and no destroy.
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            // No class brush: `WM_ERASEBKGND` fills with the theme's own, which can change
            // under an open window.
            lpszClassName: name,
            ..Default::default()
        };
        // SAFETY: a fully initialized class whose name outlives the process.
        unsafe { RegisterClassExW(&class) != 0 }
    });
    registered.then_some(name)
}

extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            // Close, Esc, and Enter all arrive here: `IsDialogMessageW` turns Esc into
            // `IDCANCEL` and Enter into the default button's id.
            let id = (wparam.0 & 0xFFFF) as i32;
            if id == ID_CLOSE || id == IDCANCEL.0 {
                // SAFETY: a live window this thread owns.
                let _ = unsafe { DestroyWindow(hwnd) };
                return LRESULT(0);
            }
        }
        WM_NOTIFY => {
            if let Some(url) = clicked_link_url(lparam) {
                open_url(&url);
                return LRESULT(0);
            }
        }
        WM_ERASEBKGND => {
            let theme = current_theme();
            let mut rect = RECT::default();
            // SAFETY: `wparam` is the `HDC` Windows passed, and the rect is an out-parameter.
            unsafe {
                let _ = GetClientRect(hwnd, &mut rect);
                FillRect(
                    HDC(wparam.0 as *mut c_void),
                    &rect,
                    dark_mode::background_brush(theme),
                );
            }
            return LRESULT(1);
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let theme = current_theme();
            let (background, text) = theme.colors();
            // SAFETY: `wparam` is the `HDC` Windows is about to draw the control with.
            unsafe {
                let hdc = HDC(wparam.0 as *mut c_void);
                SetTextColor(hdc, text);
                SetBkColor(hdc, background);
            }
            return LRESULT(dark_mode::background_brush(theme).0 as isize);
        }
        WM_SETTINGCHANGE => {
            // Someone flipped light/dark while the box was open. `lParam` names the area.
            if setting_is_color_scheme(lparam) {
                retheme(hwnd);
            }
        }
        WM_DESTROY => {
            msg_hook::unregister_dialog(hwnd);
            if let Some(window) = OPEN.with_borrow_mut(std::option::Option::take) {
                // SAFETY: fonts this module created, with no control left to reference them:
                // the children are destroyed before their parent's `WM_DESTROY` returns.
                unsafe {
                    let _ = DeleteObject(window.body_font.into());
                    let _ = DeleteObject(window.heading_font.into());
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: the default handler, with the arguments Windows gave us.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// The URL of the `SysLink` item that was clicked or entered, if that's what this notification
/// is. The URL travels in the notification, so no id lookup is needed.
fn clicked_link_url(lparam: LPARAM) -> Option<String> {
    if lparam.0 == 0 {
        return None;
    }
    // SAFETY: for `WM_NOTIFY`, `lParam` points at an `NMHDR` that outlives the handler. Reading
    // the header is safe for every notification; only the two `SysLink` codes below let us
    // widen the view to `NMLINK`.
    let header = unsafe { &*(lparam.0 as *const NMHDR) };
    if header.code != NM_CLICK && header.code != NM_RETURN {
        return None;
    }
    // SAFETY: `NM_CLICK` and `NM_RETURN` from a `SysLink` carry an `NMLINK`, whose first member
    // is that `NMHDR`.
    let link = unsafe { &*(lparam.0 as *const NMLINK) };
    let url = &link.item.szUrl;
    let end = url.iter().position(|unit| *unit == 0).unwrap_or(url.len());
    (end > 0).then(|| String::from_utf16_lossy(&url[..end]))
}

/// Hand a URL to the default browser. Only ever ours, from [`super::content`].
fn open_url(url: &str) {
    let url = HSTRING::from(url);
    // SAFETY: both strings outlive the call. `ShellExecuteW` returns a pseudo-`HINSTANCE` that
    // is an error code below 32, which we don't act on: a browser that won't start is nothing
    // this box can fix.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(url.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        log::warn!("Couldn't open {url} in a browser");
    }
}

/// Whether a `WM_SETTINGCHANGE` is the one that means light/dark changed.
fn setting_is_color_scheme(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    // SAFETY: for the changes we care about, `lParam` is a null-terminated wide string that
    // outlives the handler. A change that passes something else passes null, handled above.
    let area = unsafe { PCWSTR(lparam.0 as *const u16).to_string() };
    area.is_ok_and(|area| area == "ImmersiveColorSet")
}

/// Re-read the system theme and repaint the open box in it.
fn retheme(hwnd: HWND) {
    let theme = dark_mode::current_theme();
    OPEN.with_borrow_mut(|open| {
        if let Some(window) = open.as_mut() {
            window.theme = theme;
        }
    });
    dark_mode::apply_to_window(hwnd, theme);
    set_caption_theme(hwnd, theme);
    // SAFETY: a live window; `true` asks for the background to be erased, which is what picks
    // up the new brush.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

/// The theme the open box is painting in. Falls back to the system's answer if the box somehow
/// paints before it's recorded.
fn current_theme() -> Theme {
    OPEN.with_borrow(|open| open.as_ref().map(|window| window.theme))
        .unwrap_or_else(dark_mode::current_theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window is tall enough for everything in it. A layout constant that grows without the
    /// height following leaves the Close button under the bottom edge.
    #[test]
    fn the_client_area_fits_its_contents() {
        let bottom = layout::PADDING
            + layout::ICON
            + layout::LINE
            + layout::LINE_STEP * 2
            + (layout::LINE * 2 + 4)
            + 4
            + layout::LINE_STEP * 2
            + 4
            + layout::BUTTON_HEIGHT;
        assert!(
            client_height_at_96_dpi() >= bottom + layout::PADDING - layout::LINE_STEP,
            "the client area is {} but the last control ends at {bottom}",
            client_height_at_96_dpi()
        );
    }

    /// A link's markup is what `SysLink` parses, and the label is what a person reads.
    #[test]
    fn a_link_becomes_syslink_markup() {
        assert_eq!(
            link_markup("https://getprvw.com", "getprvw.com"),
            "<a href=\"https://getprvw.com\">getprvw.com</a>"
        );
    }
}
