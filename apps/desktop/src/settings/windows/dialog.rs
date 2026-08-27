//! The Win32 half of the settings dialog: create the windows, and turn messages back into
//! [`model::apply`] calls.
//!
//! This is the only module here a Mac can't run, so it is deliberately thin. It decides nothing
//! about what a page holds ([`model`]), where a control goes ([`layout`]), which id a control
//! carries ([`ids`]), or what colour anything is painted (`crate::chrome`, shared with the About
//! box). It creates windows at the rects it's handed and forwards what they say.
//!
//! ## The shape, and why
//!
//! - **Modeless.** `CreateDialogIndirectParamW` runs no message loop and doesn't disable the
//!   owner, so the image window stays live and the slideshow timer keeps ticking. Every modal
//!   alternative (`DialogBoxParam`, `PropertySheet` without `PSH_MODELESS`, `TaskDialogIndirect`)
//!   starves winit's pump; `platform::windows::msg_hook` has the full argument.
//! - **It registers with the message hook**, which is what makes Tab, the arrow keys, Esc, and
//!   mnemonics work: `IsDialogMessageW` has to see the messages before they're dispatched, and
//!   winit's loop doesn't call it. That seam already existed for this.
//! - **No OK, Cancel, or Apply.** One Close button. Settings apply the moment they change,
//!   which is how they work on macOS, how the RAW page's live tuning has to work, and how
//!   Windows 11's own Settings behaves.
//! - **Six pages, built once, shown and hidden.** Each is a child dialog with
//!   `WS_EX_CONTROLPARENT`, so `IsDialogMessageW` walks into it for Tab navigation.
//!
//! ## What has run
//!
//! One session, on 2026-08-27, which found both of the painting gotchas in this directory's
//! `CLAUDE.md`. Everything else is still only checked by `./scripts/check.sh --check
//! windows-cross`, which sees API shapes and no runtime behaviour, so the logic worth being sure
//! about lives in the pure modules.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DT_CALCRECT, DT_NOPREFIX, DT_WORDBREAK, DeleteObject, DrawTextW, GetDC,
    HDC, HFONT, HGDIOBJ, InvalidateRect, RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE, RedrawWindow,
    ReleaseDC, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    ICC_BAR_CLASSES, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES, INITCOMMONCONTROLSEX,
    InitCommonControlsEx, NMHDR, SetScrollInfo, TBM_SETPAGESIZE, TBM_SETPOS, TBM_SETRANGE,
    TBM_SETTICFREQ, TBS_AUTOTICKS, TBS_HORZ, TCIF_TEXT, TCITEMW, TCM_ADJUSTRECT, TCM_GETCURSEL,
    TCM_INSERTITEMW, TCM_SETCURSEL, TCN_SELCHANGE, TRACKBAR_CLASSW, WC_TABCONTROLW,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_PUSHBUTTON,
    CreateDialogIndirectParamW, CreateWindowExW, DLGTEMPLATE, DestroyWindow, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GW_OWNER, GetDlgCtrlID, GetScrollInfo, GetWindow,
    GetWindowRect, HMENU, IDCANCEL, IDOK, NONCLIENTMETRICSW, PostMessageW, SB_BOTTOM, SB_LINEDOWN,
    SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION, SB_THUMBTRACK, SB_TOP, SB_VERT,
    SCROLLINFO, SIF_ALL, SPI_GETNONCLIENTMETRICS, SW_ERASE, SW_HIDE, SW_INVALIDATE,
    SW_SCROLLCHILDREN, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, ScrollWindowEx, SendMessageW,
    SetForegroundWindow, SetWindowPos, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX,
    WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_HSCROLL, WM_NCDESTROY, WM_NEXTDLGCTL, WM_NOTIFY,
    WM_SETFONT, WM_SETTINGCHANGE, WM_VSCROLL, WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_EX_CONTROLPARENT, WS_GROUP, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

use crate::chrome::{Ink, Theme};
use crate::commands::{self, AppCommand};
use crate::parity::setting_keys::SettingKey;
use crate::parity::{Audit, Mismatch, Platform};
use crate::platform::windows::dark_mode;
use crate::platform::windows::msg_hook;
use crate::settings::Settings;

use super::ids::{self, Slot};
use super::layout::{self, Rect, ScrollState};
use super::model::{self, RowKind, Tab, Value, button};
use super::{file_types, template};

// ── Constants the `windows` crate doesn't carry ──────────────────────────────

/// `SS_LEFT`, from `winuser.h`. Left-aligned static text.
const SS_LEFT: i32 = 0x0000;
/// `SS_RIGHT`, from `winuser.h`. Right-aligned, for the number beside a trackbar.
const SS_RIGHT: i32 = 0x0002;
/// `SS_NOPREFIX`, from `winuser.h`. Without it a static eats `&` as a mnemonic marker and
/// underlines the next letter. Copy has no ampersands (`model`'s `no_copy_carries_an_ampersand`
/// keeps that true), and this is the belt to that's braces.
const SS_NOPREFIX: i32 = 0x0080;
/// `TBM_GETPOS`, which is `WM_USER + 0`.
const TBM_GETPOS: u32 = 0x0400;

/// The dialog's caption.
const TITLE: &str = "Settings";

/// Our own message, posted to ourselves when the monitor's scale changes.
///
/// `WM_DPICHANGED` can't rebuild the dialog where it arrives: Windows keeps using the window
/// after the handler returns, and every control's font, size, and position is wrong by then.
/// Posting means the rebuild happens on a later turn of the pump, with nothing above it on the
/// stack.
const WM_REBUILD_FOR_DPI: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

// ── State ────────────────────────────────────────────────────────────────────

/// One settings row's controls.
struct RowWidgets {
    key: SettingKey,
    kind: RowKind,
    /// The checkbox, trackbar, or read-only edit.
    control: HWND,
    /// The static showing a trackbar's value.
    value: Option<HWND>,
    /// The grey line under it, which scroll-to-zoom rewrites as it's toggled.
    description: HWND,
}

struct PageWindow {
    tab: Tab,
    hwnd: HWND,
    /// `Some` only for the RAW page, the one taller than the dialog.
    scroll: Option<ScrollState>,
}

/// The open dialog. One at a time, on the event loop's thread, which is the only thread allowed
/// to touch any of these handles.
struct SettingsDialog {
    hwnd: HWND,
    tabs: HWND,
    pages: Vec<PageWindow>,
    rows: Vec<RowWidgets>,
    /// The read-only field showing the custom DCP folder, refreshed when the picker comes back.
    dcp_field: Option<HWND>,
    font: HFONT,
    /// Which way it's painting right now, so a `WM_SETTINGCHANGE` can tell a real switch from
    /// the many that aren't one. No brush is held: `dark_mode::background_brush` owns one per
    /// surface per theme for the life of the process, so there's nothing here to free.
    theme: Theme,
    current: usize,
}

thread_local! {
    static DIALOG: RefCell<Option<SettingsDialog>> = const { RefCell::new(None) };
}

/// Read something out of the open dialog. The borrow never spans a call into Win32: a window
/// procedure can reach back in here, and a `RefCell` that's already borrowed would panic.
fn with_dialog<T>(read: impl FnOnce(&SettingsDialog) -> T) -> Option<T> {
    DIALOG.with_borrow(|dialog| dialog.as_ref().map(read))
}

// ── Opening ──────────────────────────────────────────────────────────────────

/// Put the settings dialog up, owned by the main window.
///
/// Returns as soon as it's on screen. A second call while it's open brings the existing one
/// forward rather than making another.
pub fn show_settings_window(owner: HWND) {
    if let Some(hwnd) = with_dialog(|dialog| dialog.hwnd) {
        // SAFETY: the handle belongs to the dialog we opened on this thread.
        let _ = unsafe { SetForegroundWindow(hwnd) };
        return;
    }

    // comctl32 v6 comes from the application manifest; this is what registers the classes we
    // ask for by name below.
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_TAB_CLASSES | ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
    };
    // SAFETY: `dwSize` declares the struct, which is all the call reads.
    let _ = unsafe { InitCommonControlsEx(&controls) };
    dark_mode::allow_dark_mode_for_app();

    let Some(dialog) = build(owner) else {
        return;
    };
    let hwnd = dialog.hwnd;
    DIALOG.replace(Some(dialog));

    // Stage 1 of the message hook: without this, Tab, the arrow keys, Esc, and the mnemonics
    // all stop working inside the dialog.
    msg_hook::register_dialog(hwnd);
    // SAFETY: a live window of ours.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    log::debug!("Settings dialog shown");
}

/// Close it, if it's open. The QA server's `CloseSettings` is the caller.
pub fn close_settings_window() {
    let Some(hwnd) = with_dialog(|dialog| dialog.hwnd) else {
        return;
    };
    // SAFETY: a live window of ours. `WM_DESTROY` does the cleanup.
    let _ = unsafe { DestroyWindow(hwnd) };
}

/// Switch to a tab by name, for the QA server's `ShowSettingsSection`.
pub fn switch_settings_section(section: &str) {
    let Some(tab) = Tab::from_section_name(section) else {
        log::warn!("Unknown settings section: {section}");
        return;
    };
    let Some((tabs, index)) = with_dialog(|dialog| {
        let index = dialog.pages.iter().position(|page| page.tab == tab)?;
        Some((dialog.tabs, index))
    })
    .flatten() else {
        log::debug!("Settings dialog isn't open, so there's no section to switch to");
        return;
    };
    // SAFETY: a live tab control of ours. `TCM_SETCURSEL` doesn't send `TCN_SELCHANGE`, which is
    // why `select_page` follows rather than waiting to be notified.
    unsafe { SendMessageW(tabs, TCM_SETCURSEL, Some(WPARAM(index)), None) };
    select_page(index);
}

/// Put the freshly picked folder in the DCP field.
///
/// Called from `app::executor` when `SetCustomDcpDir` lands, which is the event loop's thread
/// and so the dialog's own. The picker runs on a worker thread and can't touch a window.
pub fn sync_custom_dcp_dir(folder: Option<&str>) {
    let Some(Some(field)) = with_dialog(|dialog| dialog.dcp_field) else {
        return;
    };
    set_text(field, folder.unwrap_or(""));
}

// ── Building ─────────────────────────────────────────────────────────────────

fn build(owner: HWND) -> Option<SettingsDialog> {
    // SAFETY: `None` asks for this executable's own module, which always exists.
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    let instance = windows::Win32::Foundation::HINSTANCE(instance.0);

    // No `WS_VISIBLE`: the controls go on before anything is shown, so the dialog never appears
    // half-built. No `WS_THICKFRAME`: 39 settings in a fixed layout have nothing to gain from
    // being resized, and Windows 11 rounds the corners either way.
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;
    let frame = template::dialog(style.0, 0, TITLE);
    // SAFETY: the template is a well-formed `DLGTEMPLATE` with no control entries (`template`
    // owns that guarantee and its tests check the bytes), and it outlives this call.
    let hwnd = unsafe {
        CreateDialogIndirectParamW(
            Some(instance),
            frame.as_ptr().cast::<DLGTEMPLATE>(),
            Some(owner),
            Some(dialog_proc),
            LPARAM(0),
        )
    }
    .ok()?;

    // SAFETY: a live window of ours. Per-Monitor v2 is declared in the manifest, so this is the
    // DPI of the monitor the dialog opened on.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let theme = dark_mode::current_theme();
    let font = message_font(dpi);

    match populate(hwnd, instance, owner, dpi, theme, font) {
        Some(dialog) => Some(dialog),
        None => {
            log::error!("The settings dialog couldn't be built; taking the half-made one down");
            // SAFETY: the window we just created, and nothing refers to it afterwards. Its
            // children go with it, and the font is held by nothing once they have.
            unsafe {
                let _ = DestroyWindow(hwnd);
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
            None
        }
    }
}

#[allow(clippy::too_many_arguments)] // The dialog's own handles, threaded to every control.
fn populate(
    hwnd: HWND,
    instance: windows::Win32::Foundation::HINSTANCE,
    owner: HWND,
    dpi: u32,
    theme: Theme,
    font: HFONT,
) -> Option<SettingsDialog> {
    dark_mode::apply_to_window(hwnd, theme);
    size_and_centre(hwnd, owner, dpi);

    let tabs = create_tab_control(hwnd, dpi, font, theme)?;
    let page_area = tab_display_area(tabs, dpi);

    let mut audit: Audit<SettingKey> = Audit::new();
    let mut pages = Vec::new();
    let mut rows = Vec::new();
    let mut dcp_field = None;
    for tab in Tab::ALL {
        let page = build_page(
            hwnd,
            instance,
            *tab,
            page_area,
            dpi,
            font,
            theme,
            &mut audit,
            &mut rows,
            &mut dcp_field,
        )?;
        pages.push(page);
    }
    check_parity(&audit);
    grey_out_dependents(&rows, &Settings::load());

    // `IDCANCEL` rather than an id of our own, so Esc, the title bar's X, and this button all
    // arrive at the same place. `IsDialogMessageW` in the message hook is what turns Esc into
    // an `IDCANCEL` command.
    create_control(
        hwnd,
        w!("BUTTON"),
        button::CLOSE,
        WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP | WS_GROUP,
        scaled(layout::dialog::close_button_rect(), dpi),
        IDCANCEL.0,
        font,
        theme,
    )?;

    // The first page is the one the tab control already has selected.
    // SAFETY: a live child window of ours.
    unsafe {
        let _ = ShowWindow(pages[0].hwnd, SW_SHOW);
        // `WM_NEXTDLGCTL` with no target is the documented way to put focus on the first tab
        // stop. `SetFocus` on a control-parent child does nothing useful.
        SendMessageW(hwnd, WM_NEXTDLGCTL, Some(WPARAM(0)), Some(LPARAM(0)));
    }

    Some(SettingsDialog {
        hwnd,
        tabs,
        pages,
        rows,
        dcp_field,
        font,
        theme,
        current: 0,
    })
}

/// Check what the dialog built against what Windows declared it owes.
///
/// The compiler makes every platform answer for every `SettingKey`, but it can't see whether an
/// answer of `Present` came with a control. This is the runtime half, and it's the twin of the
/// macOS window's own `check_parity`.
fn check_parity(audit: &Audit<SettingKey>) {
    let mismatches = audit.mismatches(SettingKey::panel_coverage(Platform::Windows));
    for mismatch in &mismatches {
        match mismatch {
            Mismatch::Declared(key) => log::error!(
                "Settings parity: Windows declares {} present, but no control was built for it",
                key.name()
            ),
            Mismatch::Undeclared(key, coverage) => log::error!(
                "Settings parity: a control was built for {}, which Windows declares {}",
                key.name(),
                coverage.status()
            ),
        }
    }
    debug_assert!(
        mismatches.is_empty(),
        "the settings dialog and parity::setting_keys disagree: {mismatches:?}"
    );
}

#[allow(clippy::too_many_arguments)] // A straight-through factory; a struct would hide the wiring.
fn build_page(
    parent: HWND,
    instance: windows::Win32::Foundation::HINSTANCE,
    tab: Tab,
    area: Rect,
    dpi: u32,
    font: HFONT,
    theme: Theme,
    audit: &mut Audit<SettingKey>,
    rows: &mut Vec<RowWidgets>,
    dcp_field: &mut Option<HWND>,
) -> Option<PageWindow> {
    let model_page = model::page(tab);
    // `DS_CONTROL` plus `WS_EX_CONTROLPARENT` is what lets `IsDialogMessageW` walk into the page
    // for Tab navigation. Without them Tab stops at the tab control.
    let mut style = WS_CHILD | WS_CLIPCHILDREN;
    if model_page.scrolls {
        style |= WS_VSCROLL;
    }
    const DS_CONTROL: u32 = 0x0400;
    let frame = template::dialog(style.0 | DS_CONTROL, WS_EX_CONTROLPARENT.0, "");
    // SAFETY: as in `build`: a well-formed template that outlives the call, and a live parent.
    let hwnd = unsafe {
        CreateDialogIndirectParamW(
            Some(instance),
            frame.as_ptr().cast::<DLGTEMPLATE>(),
            Some(parent),
            Some(page_proc),
            LPARAM(0),
        )
    }
    .ok()?;

    // SAFETY: a live child of ours; the rect is in the parent's client coordinates.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            area.x,
            area.y,
            area.width,
            area.height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    dark_mode::apply_to_window(hwnd, theme);

    let settings = Settings::load();
    let measure = |text: &str, width: i32| measure_text(hwnd, font, text, width);
    let placed = layout::place(model_page, area.width, dpi, &measure);

    for group in &placed.groups {
        if let (Some(title), Some(frame)) = (group.title, group.frame) {
            create_control(
                hwnd,
                w!("BUTTON"),
                title,
                WINDOW_STYLE(BS_GROUPBOX as u32),
                frame,
                ids::GROUP_BOX,
                font,
                theme,
            )?;
        }
        for placed_row in &group.rows {
            let row = placed_row.row;
            if matches!(row.kind, RowKind::FileTypes) {
                // Its surface is the page's own furniture, below. The audit still records it,
                // because the capability is reachable here.
                audit.record(row.key);
                continue;
            }
            audit.record(row.key);
            let row_index = rows.len();
            let value = model::value_of(row.key, &settings);

            if let Some(title) = placed_row.title {
                create_control(
                    hwnd,
                    w!("STATIC"),
                    row.label(),
                    WINDOW_STYLE((SS_LEFT | SS_NOPREFIX) as u32),
                    title,
                    ids::control(row_index, Slot::Title),
                    font,
                    theme,
                )?;
            }

            let control = match row.kind {
                RowKind::Checkbox => {
                    let hwnd_control = create_control(
                        hwnd,
                        w!("BUTTON"),
                        row.label(),
                        WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
                        placed_row.control,
                        ids::control(row_index, Slot::Control),
                        font,
                        theme,
                    )?;
                    let on = value.as_ref().and_then(Value::as_bool).unwrap_or(false);
                    set_checked(hwnd_control, on);
                    hwnd_control
                }
                RowKind::Trackbar(scale) => {
                    let bar = create_control(
                        hwnd,
                        TRACKBAR_CLASSW,
                        "",
                        WINDOW_STYLE(TBS_HORZ | TBS_AUTOTICKS) | WS_TABSTOP,
                        placed_row.control,
                        ids::control(row_index, Slot::Control),
                        font,
                        theme,
                    )?;
                    let current = value
                        .as_ref()
                        .and_then(Value::as_number)
                        .unwrap_or(scale.min);
                    // SAFETY: a live trackbar of ours; every message below takes plain integers.
                    unsafe {
                        SendMessageW(
                            bar,
                            TBM_SETRANGE,
                            Some(WPARAM(0)),
                            Some(LPARAM(pack_range(0, scale.steps))),
                        );
                        SendMessageW(
                            bar,
                            TBM_SETTICFREQ,
                            Some(WPARAM((scale.steps / 10).max(1) as usize)),
                            None,
                        );
                        SendMessageW(
                            bar,
                            TBM_SETPAGESIZE,
                            Some(WPARAM(0)),
                            Some(LPARAM((scale.steps / 10).max(1) as isize)),
                        );
                        SendMessageW(
                            bar,
                            TBM_SETPOS,
                            Some(WPARAM(1)),
                            Some(LPARAM(scale.position(current) as isize)),
                        );
                    }
                    bar
                }
                RowKind::Folder => {
                    let field = create_control(
                        hwnd,
                        w!("EDIT"),
                        "",
                        WINDOW_STYLE((ES_AUTOHSCROLL | ES_READONLY) as u32)
                            | WS_BORDER
                            | WS_TABSTOP,
                        placed_row.control,
                        ids::control(row_index, Slot::Control),
                        font,
                        theme,
                    )?;
                    if let Some(Value::Folder(Some(folder))) = &value {
                        set_text(field, folder);
                    }
                    *dcp_field = Some(field);
                    let labels = [button::BROWSE, button::CLEAR];
                    let slots = [Slot::Browse, Slot::Clear];
                    for ((rect, label), slot) in placed_row.buttons.iter().zip(labels).zip(slots) {
                        create_control(
                            hwnd,
                            w!("BUTTON"),
                            label,
                            WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP,
                            *rect,
                            ids::control(row_index, slot),
                            font,
                            theme,
                        )?;
                    }
                    field
                }
                RowKind::FileTypes => unreachable!("handled above"),
            };

            let value_hwnd = match (placed_row.value, row.kind) {
                (Some(rect), RowKind::Trackbar(scale)) => {
                    let current = value
                        .as_ref()
                        .and_then(Value::as_number)
                        .unwrap_or(scale.min);
                    Some(create_control(
                        hwnd,
                        w!("STATIC"),
                        &scale.render(current),
                        WINDOW_STYLE((SS_RIGHT | SS_NOPREFIX) as u32),
                        rect,
                        ids::control(row_index, Slot::Value),
                        font,
                        theme,
                    )?)
                }
                _ => None,
            };

            // Scroll-to-zoom's line says something different depending on the setting, so it's
            // read from the model rather than from the row.
            let description = if row.key == SettingKey::ScrollToZoom {
                model::scroll_to_zoom_description(settings.scroll_to_zoom)
            } else {
                row.description
            };
            let description_hwnd = create_control(
                hwnd,
                w!("STATIC"),
                description,
                WINDOW_STYLE((SS_LEFT | SS_NOPREFIX) as u32),
                placed_row.description,
                ids::control(row_index, Slot::Description),
                font,
                theme,
            )?;

            rows.push(RowWidgets {
                key: row.key,
                kind: row.kind,
                control,
                value: value_hwnd,
                description: description_hwnd,
            });
        }
    }

    if let Some(file_types) = &placed.file_types {
        build_file_types_page(hwnd, file_types, font, theme)?;
    }
    if let Some(reset) = placed.reset_button {
        create_control(
            hwnd,
            w!("BUTTON"),
            button::RESET,
            WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP,
            reset,
            ids::RESET,
            font,
            theme,
        )?;
    }

    let scroll = model_page
        .scrolls
        .then(|| ScrollState::new(placed.height, area.height));
    if let Some(state) = scroll {
        debug_assert!(
            state.scrollable(),
            "the RAW page is the only page with a scroll bar, and it's meant to overflow"
        );
        set_scroll_bar(hwnd, state);
    }

    Some(PageWindow { tab, hwnd, scroll })
}

/// Grey out the rows that hang off a switched-off toggle, once every page exists.
///
/// The one cross-dependency the settings have: matching the display and picking a rendering
/// intent are both steps inside the ICC transform, so neither means anything with colour
/// management off.
fn grey_out_dependents(rows: &[RowWidgets], settings: &Settings) {
    for row in rows {
        let dependents = model::dependents(row.key);
        if dependents.is_empty() {
            continue;
        }
        let on = model::value_of(row.key, settings)
            .as_ref()
            .and_then(Value::as_bool)
            .unwrap_or(true);
        for target in rows
            .iter()
            .filter(|target| dependents.contains(&target.key))
        {
            // SAFETY: a live control of ours.
            let _ = unsafe { EnableWindow(target.control, on) };
        }
    }
}

/// The File associations page's own furniture: the extension list and the two buttons.
fn build_file_types_page(
    parent: HWND,
    placed: &layout::PlacedFileTypes,
    font: HFONT,
    theme: Theme,
) -> Option<()> {
    let page = model::page(Tab::FileAssociations);
    let explanation = page.rows().next()?.description;
    create_control(
        parent,
        w!("STATIC"),
        explanation,
        WINDOW_STYLE((SS_LEFT | SS_NOPREFIX) as u32),
        placed.explanation,
        ids::FILE_TYPE_EXPLANATION,
        font,
        theme,
    )?;
    create_control(
        parent,
        w!("EDIT"),
        &file_types::extension_list_text(),
        WINDOW_STYLE((ES_READONLY | ES_MULTILINE | ES_AUTOVSCROLL) as u32)
            | WS_BORDER
            | WS_TABSTOP
            | WS_VSCROLL,
        placed.list,
        ids::FILE_TYPE_LIST,
        font,
        theme,
    )?;
    create_control(
        parent,
        w!("BUTTON"),
        button::REGISTER_FILE_TYPES,
        WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP,
        placed.register_button,
        ids::REGISTER_FILE_TYPES,
        font,
        theme,
    )?;
    create_control(
        parent,
        w!("BUTTON"),
        button::OPEN_DEFAULT_APPS,
        WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP,
        placed.windows_settings_button,
        ids::OPEN_DEFAULT_APPS,
        font,
        theme,
    )?;
    Some(())
}

// ── Small Win32 helpers ──────────────────────────────────────────────────────

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[allow(clippy::too_many_arguments)] // One call per control kind; the arguments are the control.
fn create_control(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    rect: Rect,
    id: i32,
    font: HFONT,
    theme: Theme,
) -> Option<HWND> {
    let text = wide(text);
    // SAFETY: `parent` is a live window of ours, `class` is a registered class (comctl32's are
    // registered by `InitCommonControlsEx` and the four built-ins always exist), and `text`
    // outlives the call, which copies it.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR(text.as_ptr()),
            style | WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            Some(parent),
            Some(HMENU(id as isize as *mut c_void)),
            None,
            None,
        )
    }
    .inspect_err(|error| log::error!("Couldn't create a settings control: {error}"))
    .ok()?;

    // Not `GetStockObject(DEFAULT_GUI_FONT)`, which Microsoft deprecates for this: it's still
    // the 1995 System font and looks it.
    // SAFETY: a live control and a live font; `WM_SETFONT` takes ownership of neither.
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        )
    };
    dark_mode::apply_to_window(hwnd, theme);
    Some(hwnd)
}

fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    // SAFETY: a live control of ours; the string outlives the call.
    let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(text.as_ptr())) };
}

fn set_checked(hwnd: HWND, on: bool) {
    // SAFETY: a live checkbox of ours. `BST_CHECKED` is 1 and `BST_UNCHECKED` is 0.
    unsafe { SendMessageW(hwnd, BM_SETCHECK, Some(WPARAM(usize::from(on))), None) };
}

fn is_checked(hwnd: HWND) -> bool {
    // SAFETY: a live checkbox of ours.
    unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None) }.0 == 1
}

/// `MAKELONG(low, high)`, which is how `TBM_SETRANGE` takes both ends at once.
const fn pack_range(low: i32, high: i32) -> isize {
    ((low as u16 as u32) | ((high as u16 as u32) << 16)) as isize
}

fn scaled(rect: Rect, dpi: u32) -> Rect {
    Rect {
        x: layout::scale(rect.x, dpi),
        y: layout::scale(rect.y, dpi),
        width: layout::scale(rect.width, dpi),
        height: layout::scale(rect.height, dpi),
    }
}

/// The font Windows uses for its own UI text, at this monitor's scale.
fn message_font(dpi: u32) -> HFONT {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `cbSize` declares the buffer and the pointer is to that same struct. The `ForDpi`
    // form is what returns a height already scaled for the monitor, which is the whole reason
    // to prefer it over `SystemParametersInfoW`.
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
        log::warn!("Couldn't read the system message font; the dialog gets the default one");
    }
    // SAFETY: a `LOGFONTW` we own, zeroed if the read failed, which asks for a default font.
    unsafe { CreateFontIndirectW(&metrics.lfMessageFont) }
}

/// How tall `text` is once it wraps to `width`, in device pixels, in the dialog's own font.
fn measure_text(hwnd: HWND, font: HFONT, text: &str, width: i32) -> i32 {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: 0,
    };
    let mut text: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: a live window of ours; the DC is released below and the font is put back first.
    unsafe {
        let hdc: HDC = GetDC(Some(hwnd));
        if hdc.is_invalid() {
            return 0;
        }
        let previous = SelectObject(hdc, HGDIOBJ(font.0));
        DrawTextW(
            hdc,
            &mut text,
            &mut rect,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        SelectObject(hdc, previous);
        ReleaseDC(Some(hwnd), hdc);
    }
    rect.bottom - rect.top
}

/// Size the dialog for this DPI and put it over the middle of the main window.
fn size_and_centre(hwnd: HWND, owner: HWND, dpi: u32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: layout::scale(layout::dialog::WIDTH, dpi),
        bottom: layout::scale(layout::dialog::HEIGHT, dpi),
    };
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    // SAFETY: a `RECT` we own. The `ForDpi` form is what gets the frame's own thickness right
    // on a scaled monitor.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut rect, style, false, WINDOW_EX_STYLE(0), dpi) };
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;

    let mut owner_rect = RECT::default();
    // SAFETY: `owner` is winit's live main window.
    let centred = unsafe { GetWindowRect(owner, &mut owner_rect) }.is_ok();
    let (x, y) = if centred {
        (
            owner_rect.left + (owner_rect.right - owner_rect.left - width) / 2,
            owner_rect.top + (owner_rect.bottom - owner_rect.top - height) / 2,
        )
    } else {
        (rect.left, rect.top)
    };

    // SAFETY: a live window of ours.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

fn create_tab_control(parent: HWND, dpi: u32, font: HFONT, theme: Theme) -> Option<HWND> {
    let tabs = create_control(
        parent,
        WC_TABCONTROLW,
        "",
        WS_TABSTOP | WS_GROUP,
        scaled(layout::dialog::tab_rect(), dpi),
        ids::TAB,
        font,
        theme,
    )?;
    for (index, tab) in Tab::ALL.iter().enumerate() {
        let mut title = wide(tab.title());
        let item = TCITEMW {
            mask: TCIF_TEXT,
            pszText: windows::core::PWSTR(title.as_mut_ptr()),
            ..Default::default()
        };
        // SAFETY: a live tab control of ours; `title` outlives the call, which copies the text.
        unsafe {
            SendMessageW(
                tabs,
                TCM_INSERTITEMW,
                Some(WPARAM(index)),
                Some(LPARAM(std::ptr::from_ref(&item) as isize)),
            )
        };
    }
    Some(tabs)
}

/// Where a page goes: the tab control's rect, minus the tabs themselves and the border.
fn tab_display_area(tabs: HWND, dpi: u32) -> Rect {
    let outer = scaled(layout::dialog::tab_rect(), dpi);
    let mut rect = RECT {
        left: outer.x,
        top: outer.y,
        right: outer.right(),
        bottom: outer.bottom(),
    };
    // SAFETY: a live tab control of ours; `TCM_ADJUSTRECT` reads and writes the `RECT` behind
    // `lParam` and nothing else.
    unsafe {
        SendMessageW(
            tabs,
            TCM_ADJUSTRECT,
            Some(WPARAM(0)),
            Some(LPARAM(std::ptr::from_mut(&mut rect) as isize)),
        )
    };
    Rect {
        x: rect.left,
        y: rect.top,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
    }
}

fn set_scroll_bar(page: HWND, state: ScrollState) {
    let info = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_ALL,
        nMin: 0,
        nMax: state.content.max(1) - 1,
        nPage: state.visible.max(1) as u32,
        nPos: state.position,
        nTrackPos: 0,
    };
    // SAFETY: `cbSize` declares the struct, and `page` is a live child of ours with `WS_VSCROLL`.
    unsafe { SetScrollInfo(page, SB_VERT, &info, true) };
}

// ── Messages ─────────────────────────────────────────────────────────────────

/// The main dialog's procedure: the tab control, the Close button, and the theme.
///
/// Returning `TRUE` means "handled"; the dialog manager does the rest.
unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match message {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            if id == IDCANCEL.0 || id == IDOK.0 {
                // SAFETY: a live window of ours. `WM_DESTROY` cleans up.
                let _ = unsafe { DestroyWindow(hwnd) };
                return 1;
            }
            0
        }
        WM_NOTIFY => {
            // SAFETY: `WM_NOTIFY`'s `lParam` is documented as an `NMHDR` that outlives the send.
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            if header.code == TCN_SELCHANGE && header.idFrom as i32 == ids::TAB {
                let Some(tabs) = with_dialog(|dialog| dialog.tabs) else {
                    return 0;
                };
                // SAFETY: a live tab control of ours.
                let selected = unsafe { SendMessageW(tabs, TCM_GETCURSEL, None, None) }.0;
                if selected >= 0 {
                    select_page(selected as usize);
                }
                return 1;
            }
            0
        }
        WM_CTLCOLORDLG | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT
        | WM_CTLCOLORLISTBOX => paint_control(wparam, lparam),
        // The monitor's scale changed, or the dialog was dragged to one at a different scale.
        // Everything on it was sized for the old DPI, and it's all built from data, so the
        // honest answer is to build it again rather than to walk it resizing controls.
        WM_DPICHANGED => {
            // SAFETY: a live window of ours; a failed post just leaves the dialog as it is.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_REBUILD_FOR_DPI, WPARAM(0), LPARAM(0)) };
            0
        }
        WM_REBUILD_FOR_DPI => {
            rebuild_for_dpi(hwnd);
            1
        }
        WM_SETTINGCHANGE => {
            // `lParam` names what changed. `"ImmersiveColorSet"` is the theme; everything else
            // arriving here belongs to somebody else.
            if immersive_color_set(lparam) {
                retheme();
            }
            0
        }
        WM_CLOSE => {
            // SAFETY: a live window of ours.
            let _ = unsafe { DestroyWindow(hwnd) };
            1
        }
        // `WM_NCDESTROY` rather than `WM_DESTROY`: `WM_DESTROY` reaches the parent *before* its
        // children, which still hold the font. `WM_NCDESTROY` is the last message a window ever
        // gets, after every child is gone. The background brush isn't ours to free —
        // `dark_mode` keeps one of each for the life of the process.
        WM_NCDESTROY => {
            msg_hook::unregister_dialog(hwnd);
            if let Some(dialog) = DIALOG.replace(None) {
                // SAFETY: the font was created by this module and nothing holds it any more.
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(dialog.font.0));
                }
            }
            log::debug!("Settings dialog closed");
            0
        }
        _ => 0,
    }
}

/// A page's procedure: the controls on it, and its scroll bar.
unsafe extern "system" fn page_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match message {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as i32;
            // `BN_CLICKED`. A button only sends the focus notifications with `BS_NOTIFY`, which
            // none of ours has, but reading the code means a read-only edit's `EN_*` can't be
            // mistaken for a click either.
            if (wparam.0 >> 16) as u16 == 0 {
                command(id, HWND(lparam.0 as *mut c_void));
            }
            0
        }
        WM_HSCROLL => {
            let code = (wparam.0 & 0xffff) as u32;
            trackbar_moved(HWND(lparam.0 as *mut c_void), code);
            0
        }
        WM_VSCROLL => {
            let code = (wparam.0 & 0xffff) as u32;
            scrolled(hwnd, code);
            0
        }
        WM_CTLCOLORDLG | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT
        | WM_CTLCOLORLISTBOX => paint_control(wparam, lparam),
        _ => 0,
    }
}

/// Answer a `WM_CTLCOLOR*`, whichever of them it was.
///
/// The dialog gives one reply to all five it hears (the sixth, `WM_CTLCOLORSCROLLBAR`, is for
/// scroll-bar controls, and the RAW page's bar is a window's own), because the message is the
/// wrong thing to key on: a read-only edit sends `WM_CTLCOLORSTATIC` and arrives indistinguishable from a
/// label. `dark_mode::paint_control` asks the control's class instead. All this adds is which
/// ink the text takes, which is the one thing the class can't say: a description and the title
/// above it are both statics, and only one of them is grey.
fn paint_control(wparam: WPARAM, lparam: LPARAM) -> isize {
    let Some(theme) = with_dialog(|dialog| dialog.theme) else {
        return 0;
    };
    // SAFETY: `lParam` on a `WM_CTLCOLOR*` is the control's own window, or the dialog itself for
    // `WM_CTLCOLORDLG`.
    let control = HWND(lparam.0 as *mut c_void);
    let id = if control.is_invalid() {
        0
    } else {
        // SAFETY: a live window of ours. A window that isn't a control answers 0, which is
        // none of our rows.
        unsafe { GetDlgCtrlID(control) }
    };
    let ink = if ids::is_secondary(id) {
        Ink::Secondary
    } else {
        Ink::Body
    };
    // SAFETY: `wParam` on a `WM_CTLCOLOR*` is the device context the control is about to draw
    // into, valid for the duration of the message.
    dark_mode::paint_control(HDC(wparam.0 as *mut c_void), control, theme, ink)
}

/// Show one page and hide the rest. `TCM_SETCURSEL` doesn't notify, so this is called both from
/// the notification and from [`switch_settings_section`].
fn select_page(index: usize) {
    let Some(pages) = with_dialog(|dialog| {
        dialog
            .pages
            .iter()
            .map(|page| page.hwnd)
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    if index >= pages.len() {
        return;
    }
    for (position, page) in pages.iter().enumerate() {
        // SAFETY: live children of ours.
        unsafe {
            let _ = ShowWindow(*page, if position == index { SW_SHOW } else { SW_HIDE });
        }
    }
    DIALOG.with_borrow_mut(|dialog| {
        if let Some(dialog) = dialog.as_mut() {
            dialog.current = index;
        }
    });
    // Focus deliberately stays on the tab control, which is what a Windows tabbed dialog does:
    // the arrow keys keep walking the tabs and Tab moves into the page.
}

/// A button or a checkbox was clicked.
fn command(id: i32, control: HWND) {
    match id {
        ids::RESET => {
            let settings = Settings::load();
            let change = model::reset_raw(&settings);
            let reset = change.settings.clone();
            deliver(change);
            refresh_controls(&reset);
            return;
        }
        ids::REGISTER_FILE_TYPES => {
            register_file_types();
            return;
        }
        ids::OPEN_DEFAULT_APPS => {
            open_default_apps_settings();
            return;
        }
        _ => {}
    }

    let Some((index, slot)) = ids::row(id) else {
        return;
    };
    let Some((key, kind)) =
        with_dialog(|dialog| dialog.rows.get(index).map(|row| (row.key, row.kind))).flatten()
    else {
        return;
    };

    match slot {
        Slot::Control if matches!(kind, RowKind::Checkbox) => {
            let on = is_checked(control);
            apply(key, Value::Bool(on));
            update_dependents(key, on);
            if key == SettingKey::ScrollToZoom {
                update_description(index, model::scroll_to_zoom_description(on));
            }
        }
        Slot::Browse => pick_dcp_folder(),
        Slot::Clear => {
            if let Some(field) = with_dialog(|dialog| dialog.dcp_field).flatten() {
                set_text(field, "");
            }
            apply(key, Value::Folder(None));
        }
        _ => {}
    }
}

/// A trackbar moved.
///
/// The label follows every message so the number tracks the drag, but the setting is only
/// written on a discrete step or when the thumb is let go. That's the same call the macOS RAW
/// sliders make, for the same reason: each RAW change costs a full decode, and a drag across
/// the track would otherwise queue a hundred of them.
fn trackbar_moved(bar: HWND, code: u32) {
    let Some((index, key, scale)) = with_dialog(|dialog| {
        dialog.rows.iter().enumerate().find_map(|(index, row)| {
            let RowKind::Trackbar(scale) = row.kind else {
                return None;
            };
            (row.control == bar).then_some((index, row.key, scale))
        })
    })
    .flatten() else {
        return;
    };

    // SAFETY: a live trackbar of ours.
    let position = unsafe { SendMessageW(bar, TBM_GETPOS, None, None) }.0 as i32;
    let value = scale.value(position);
    if let Some(Some(label)) =
        with_dialog(|dialog| dialog.rows.get(index).and_then(|row| row.value))
    {
        set_text(label, &scale.render(value));
    }

    let settled = code != SB_THUMBTRACK.0 as u32;
    if settled {
        apply(key, Value::Number(value));
    }
}

/// The RAW page's scroll bar moved.
fn scrolled(page: HWND, code: u32) {
    let Some(index) = with_dialog(|dialog| {
        dialog
            .pages
            .iter()
            .position(|candidate| candidate.hwnd == page)
    })
    .flatten() else {
        return;
    };

    let mut moved = 0;
    let mut updated = None;
    DIALOG.with_borrow_mut(|dialog| {
        let Some(dialog) = dialog.as_mut() else {
            return;
        };
        let Some(state) = dialog.pages[index].scroll.as_mut() else {
            return;
        };
        moved = match code {
            LINE_UP => state.line(false),
            LINE_DOWN => state.line(true),
            PAGE_UP => state.page(false),
            PAGE_DOWN => state.page(true),
            TO_TOP => state.scroll_to(0),
            TO_BOTTOM => state.scroll_to(state.max()),
            THUMB_RELEASED => {
                let mut info = SCROLLINFO {
                    cbSize: size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_ALL,
                    ..Default::default()
                };
                // SAFETY: `cbSize` declares the struct and `page` is a live child of ours.
                let read = unsafe { GetScrollInfo(page, SB_VERT, &mut info) };
                if read.is_ok() {
                    state.scroll_to(info.nTrackPos)
                } else {
                    0
                }
            }
            _ => 0,
        };
        updated = Some(*state);
    });

    if moved == 0 {
        return;
    }
    // SAFETY: a live child of ours. `SW_SCROLLCHILDREN` is what moves the controls with the
    // content, which is the whole point: the page has no painting of its own.
    unsafe {
        ScrollWindowEx(
            page,
            0,
            moved,
            None,
            None,
            None,
            None,
            SW_SCROLLCHILDREN | SW_INVALIDATE | SW_ERASE,
        );
        let _ = InvalidateRect(Some(page), None, true);
    }
    if let Some(state) = updated {
        set_scroll_bar(page, state);
    }
}

// The scroll-bar notification codes, as the plain `u32`s a `WM_VSCROLL` carries in its low word.
const LINE_UP: u32 = SB_LINEUP.0 as u32;
const LINE_DOWN: u32 = SB_LINEDOWN.0 as u32;
const PAGE_UP: u32 = SB_PAGEUP.0 as u32;
const PAGE_DOWN: u32 = SB_PAGEDOWN.0 as u32;
const TO_TOP: u32 = SB_TOP.0 as u32;
const TO_BOTTOM: u32 = SB_BOTTOM.0 as u32;
/// The thumb was let go. `SB_THUMBTRACK` (the drag itself) deliberately isn't handled here: the
/// content follows on release, which is one repaint rather than a hundred.
const THUMB_RELEASED: u32 = SB_THUMBPOSITION.0 as u32;

// ── Applying ─────────────────────────────────────────────────────────────────

/// Fold a control's new value into the settings and send it on its way.
///
/// The settings are re-read from disk each time rather than held, so a change made through the
/// menu bar while the dialog is open isn't clobbered by the next click in it. The macOS window
/// does the same.
fn apply(key: SettingKey, value: Value) {
    let settings = Settings::load();
    let Some(change) = model::apply(key, &value, &settings) else {
        log::warn!("The settings dialog has no way to apply {}", key.name());
        return;
    };
    deliver(change);
}

fn deliver(change: model::Change) {
    match change.command {
        // `app::executor` writes the settings file for every command that carries one.
        Some(command) => {
            commands::send_command(command);
        }
        // Nothing in the running app reads it, so persisting is the whole job.
        None => change.settings.save(),
    }
}

fn update_dependents(key: SettingKey, on: bool) {
    let dependents = model::dependents(key);
    if dependents.is_empty() {
        return;
    }
    let Some(targets) = with_dialog(|dialog| {
        dialog
            .rows
            .iter()
            .filter(|row| dependents.contains(&row.key))
            .map(|row| row.control)
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    for target in targets {
        // SAFETY: live controls of ours.
        let _ = unsafe { EnableWindow(target, on) };
    }
}

fn update_description(index: usize, text: &str) {
    if let Some(Some(description)) =
        with_dialog(|dialog| dialog.rows.get(index).map(|row| row.description))
    {
        set_text(description, text);
    }
}

/// Put every control back to what the settings now say. The RAW page's Reset button is the
/// caller, and it's written against every row rather than that page's so it stays right if a
/// second page ever grows one.
fn refresh_controls(settings: &Settings) {
    let Some(rows) = with_dialog(|dialog| {
        dialog
            .rows
            .iter()
            .map(|row| (row.key, row.kind, row.control, row.value))
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    for (key, kind, control, value) in rows {
        let Some(current) = model::value_of(key, settings) else {
            continue;
        };
        match (kind, current) {
            (RowKind::Checkbox, Value::Bool(on)) => set_checked(control, on),
            (RowKind::Trackbar(scale), Value::Number(number)) => {
                // SAFETY: a live trackbar of ours; `wParam` of 1 asks it to redraw.
                unsafe {
                    SendMessageW(
                        control,
                        TBM_SETPOS,
                        Some(WPARAM(1)),
                        Some(LPARAM(scale.position(number) as isize)),
                    )
                };
                if let Some(label) = value {
                    set_text(label, &scale.render(number));
                }
            }
            _ => {}
        }
    }
}

/// Close the dialog and open it again at the monitor's new scale, on the same tab.
///
/// It's cheap: six pages of controls, built from tables. And it's the one way to be sure every
/// font, rect, and group box is right, rather than the subset a hand-written resize remembers.
fn rebuild_for_dpi(hwnd: HWND) {
    // SAFETY: a live window of ours. A modeless dialog's owner is the window it was created
    // with, which is winit's.
    let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
    let Ok(owner) = owner else {
        log::debug!("The settings dialog has no owner to reopen against");
        return;
    };
    let section = with_dialog(|dialog| {
        dialog
            .pages
            .get(dialog.current)
            .map(|page| page.tab.title().to_string())
    })
    .flatten();

    // `DestroyWindow` runs `WM_NCDESTROY` inline, which clears `DIALOG`, so the reopen below
    // builds a new one rather than bringing this one forward.
    // SAFETY: a live window of ours, and nothing here touches it afterwards.
    let _ = unsafe { DestroyWindow(hwnd) };
    show_settings_window(owner);
    if let Some(section) = section {
        switch_settings_section(&section);
    }
}

/// Whether a `WM_SETTINGCHANGE` is the one that means "light or dark changed".
fn immersive_color_set(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    // SAFETY: when it's non-null, `WM_SETTINGCHANGE`'s `lParam` is a null-terminated wide string
    // that lives for the duration of the message.
    let name = unsafe { PCWSTR(lparam.0 as *const u16).to_string() };
    name.is_ok_and(|name| name == "ImmersiveColorSet")
}

/// Re-read the theme and repaint. `WM_SETTINGCHANGE` with `"ImmersiveColorSet"` is how a
/// light-to-dark switch reaches us.
fn retheme() {
    let theme = dark_mode::current_theme();
    let Some((hwnd, was)) = with_dialog(|dialog| (dialog.hwnd, dialog.theme)) else {
        return;
    };
    if theme == was {
        return;
    }
    DIALOG.with_borrow_mut(|dialog| {
        if let Some(dialog) = dialog.as_mut() {
            dialog.theme = theme;
        }
    });
    dark_mode::apply_to_tree(hwnd, theme);
    // `RDW_ALLCHILDREN`, because the dialog and its pages are `WS_CLIPCHILDREN`: invalidating
    // the dialog alone repaints the background the controls sit on and leaves every control
    // still painted the old way.
    // SAFETY: a live window of ours; both optional arguments mean "the whole window".
    unsafe {
        let _ = RedrawWindow(
            Some(hwnd),
            None,
            None,
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
        );
    }
}

// ── The File associations buttons ────────────────────────────────────────────

/// Write the ProgID and the `OpenWithProgids` entries, which is what puts Prvw in Explorer's
/// "Open with" list. It cannot make Prvw the default; only the user can (`file_types`).
fn register_file_types() {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegSetValueExW,
    };

    let Ok(executable) = std::env::current_exe() else {
        log::error!("Couldn't find this executable, so the file types can't be registered");
        return;
    };
    // The shell can't run a verbatim `\\?\` path, and `paths::shell_path` is what says whether
    // there's a plain spelling to use.
    let Some(executable) = crate::paths::shell_path(&executable) else {
        log::error!("This executable's path has no plain Win32 spelling to register");
        return;
    };

    let mut written = 0usize;
    for value in file_types::registration(std::path::Path::new(&executable)) {
        let key_name = wide(&value.key);
        let mut key = HKEY::default();
        // SAFETY: a constant root key, a null-terminated name that outlives the call, and an
        // out-parameter we own. `KEY_WRITE` under `HKEY_CURRENT_USER` needs no elevation.
        let opened = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(key_name.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
        };
        if opened.is_err() {
            log::warn!("Couldn't create {}: {opened:?}", value.key);
            continue;
        }

        let data = wide(&value.value);
        let bytes: &[u8] = bytemuck::cast_slice(&data);
        let name = value.name.as_deref().map(wide);
        let name_ptr = name
            .as_ref()
            .map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
        // SAFETY: `key` is the key just opened, `name_ptr` is null (the default value) or a
        // null-terminated name that outlives the call, and `bytes` is the UTF-16 string
        // including its terminator, which is what `REG_SZ` wants.
        let set = unsafe { RegSetValueExW(key, name_ptr, None, REG_SZ, Some(bytes)) };
        if set.is_err() {
            log::warn!("Couldn't write {} / {:?}: {set:?}", value.key, value.name);
        } else {
            written += 1;
        }
        // SAFETY: the key we opened, used for nothing else after this.
        let _ = unsafe { RegCloseKey(key) };
    }
    log::info!("Registered Prvw's file types: {written} registry values written");
}

/// Open the Windows Settings page where the user picks their default apps. This is the only way
/// a default handler changes on Windows 10 20H2 and later.
fn open_default_apps_settings() {
    let uri = wide(file_types::DEFAULT_APPS_URI);
    // SAFETY: both strings outlive the call. `ShellExecuteW` on a `ms-settings:` URI hands off
    // to the Settings app and returns; it opens no loop of ours.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(uri.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOW,
        )
    };
    // Anything at or below 32 is an error code rather than an instance handle.
    if result.0 as usize <= 32 {
        log::warn!("Couldn't open Windows' default apps settings");
    }
}

/// Put the folder picker up for the custom DCP directory.
///
/// On a worker thread, exactly like `open_dialog::show` and for the same reason: `IFileDialog`
/// is modal, and a modal on the event-loop thread freezes winit's pump. The chosen folder comes
/// back as an `AppCommand`, and `app::executor` calls [`sync_custom_dcp_dir`] from the event
/// loop's own thread to put it in the field.
fn pick_dcp_folder() {
    let picking = rfd::AsyncFileDialog::new()
        .set_title("Choose a folder of DCP profiles")
        .pick_folder();
    std::thread::spawn(move || {
        let Some(folder) = pollster::block_on(picking) else {
            log::debug!("The DCP folder picker was dismissed");
            return;
        };
        let path = folder.path().to_string_lossy().to_string();
        log::info!("Custom DCP directory: {path}");
        commands::send_command(AppCommand::SetCustomDcpDir(Some(path)));
    });
}
