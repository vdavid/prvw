//! The Win32 layer of browse mode: create the windows, and turn their notifications back into
//! commands.
//!
//! The only part of `browser::windows` a Mac can't run, so it is deliberately thin. It decides
//! nothing about what the tree shows ([`super::roots`]), where a pane goes ([`super::layout`]),
//! what the status bar says ([`super::status`]), or which keys it takes ([`super::keys`]). It
//! creates windows at the rects it's handed and forwards what they say.
//!
//! ## The shape, and why
//!
//! - **One container child window covering the whole client area**, holding the tree, the grid,
//!   and the status bar. Browse mode shows it and image mode hides it; the two screens **swap**
//!   rather than composite. That's the same rule the macOS browser lives by, for a different
//!   reason on each platform: a transparent Metal pixel occludes what's behind it, and a DXGI
//!   flip-model swapchain composes as its own DWM visual rather than respecting GDI's
//!   child-window clipping. One window to show and hide is safe by construction either way.
//! - **No `Present` while the browser is up.** `App` clears the image and stops asking for
//!   redraws on the way into browse mode, so the swapchain never paints over the controls, and
//!   the GPU goes idle — which is what `AGENTS.md`'s "respect resources" asks for.
//! - **The splitter is drawn by the container, not by a window of its own.** Win32 has no
//!   splitter control, and the gap between the two panes is already the container's own client
//!   area, so the mouse messages arrive here without a fourth window to own them.
//! - **No nested message loop, anywhere.** Nothing here is modal, so there is nothing to starve
//!   winit's pump with (`platform::windows::msg_hook`).
//!
//! ## What has never run
//!
//! All of it. Nothing in this module has executed on Windows. Every call is against documented
//! behaviour and the API shapes are checked by `./scripts/check.sh --check windows-cross`, but
//! the runtime is unproven, which is why the parts worth being sure about are next door and
//! tested.

mod grid;
mod tree;

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    FillRect, HBRUSH, HDC, HFONT, InvalidateRect, ScreenToClient, SetBkColor, SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, ICC_TREEVIEW_CLASSES,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, NMHDR, STATUSCLASSNAMEW,
};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, GetClientRect, GetCursorPos, GetWindowRect,
    HCURSOR, HICON, HMENU, IDC_SIZEWE, LoadCursorW, NONCLIENTMETRICSW, RegisterClassW,
    SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW,
    SetCursor, SetWindowPos, ShowWindow, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_NOTIFY, WM_SETCURSOR, WM_SETFONT,
    WM_SIZE, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::browser::PaneSide;
use crate::platform::windows::dark_mode::{self, Theme};

use super::layout::{self, Metrics, Rect};
use super::{keys, status};

/// The container's window class. Registered once per process, on the event loop's thread.
const CLASS_NAME: PCWSTR = w!("PrvwBrowseContainer");

/// Child-window ids, so a `WM_NOTIFY` says which control it came from without comparing handles.
const ID_TREE: i32 = 1001;
const ID_GRID: i32 = 1002;
const ID_STATUS: i32 = 1003;

/// The status bar's three panes, left to right.
const STATUS_PARTS: usize = 3;

/// How wide the count and size panes are, in logical pixels. The name pane takes the rest,
/// because a file name is the one field with no bound on its length.
const STATUS_COUNT_WIDTH: i32 = 130;
const STATUS_SIZE_WIDTH: i32 = 130;

/// `SB_SETPARTS`, `SB_SETTEXTW`, from `commctrl.h`. `WM_USER` is `0x0400`.
const SB_SETPARTS: u32 = 0x0400 + 4;
const SB_SETTEXTW: u32 = 0x0400 + 11;

/// Everything browse mode owns on Windows. One per window, on the event loop's thread, which is
/// the only thread allowed to touch any of these handles.
pub(super) struct Ui {
    /// The child window covering the client area; every other control is inside it.
    container: HWND,
    /// `SysTreeView32`, the folder tree.
    tree: HWND,
    /// `SysListView32` in icon mode, the thumbnail grid.
    grid: HWND,
    /// `msctls_statusbar32` along the bottom.
    status: HWND,
    /// The message font, at this monitor's DPI. Owned here and deleted with the window.
    font: HFONT,
    /// Which way the controls are painting. Read once per build; a system theme flip rebuilds.
    theme: Theme,
    /// The monitor's DPI, from `GetDpiForWindow`.
    dpi: u32,
    /// The tree pane's width in device pixels, as the user last dragged it.
    tree_width: i32,
    /// Splitter, minimum-pane, and status-bar measurements at `dpi`.
    metrics: Metrics,
    /// How far into the splitter a drag started, while one is in progress.
    drag: Option<i32>,
    /// The tree's own state: roots, the child-load cache, and the reveal walk.
    tree_state: tree::TreeState,
    /// The grid's own state: the model, the scheduler, the thumbnail cache, and the workers.
    grid_state: grid::GridState,
    /// Which pane the status bar's "Loading…" is speaking for, and the selected image's measured
    /// size once a header read lands for it.
    selected_dimensions: Option<(u32, u32)>,
}

thread_local! {
    /// The open browser. `None` until browse mode is entered for the first time; the handles then
    /// live for the window's lifetime, the same way the macOS split view does.
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Read something out of the browser, if it's been built. The borrow never spans a call into
/// Win32: a window procedure can reach back in here, and a `RefCell` already borrowed panics.
fn with_ui<T>(read: impl FnOnce(&Ui) -> T) -> Option<T> {
    UI.with_borrow(|ui| ui.as_ref().map(read))
}

/// Mutate the browser, if it's been built. Same borrow rule as [`with_ui`].
fn with_ui_mut<T>(write: impl FnOnce(&mut Ui) -> T) -> Option<T> {
    UI.with_borrow_mut(|ui| ui.as_mut().map(write))
}

/// The handle `browser::State` holds. It owns nothing: the windows and the models live in the
/// thread-local above, because a window procedure has to reach them and can't borrow through a
/// struct the caller is holding.
pub struct BrowseUi {
    /// The winit window the container is a child of, so a rebuild knows where to hang it.
    owner: HWND,
}

impl BrowseUi {
    /// Build the browser inside `owner`'s client area, hidden. Returns `None` when a control
    /// couldn't be created, in which case Enter does nothing rather than leaving a half-made
    /// window on screen.
    pub fn create(owner: HWND, sort_by: crate::navigation::SortBy) -> Option<Self> {
        if with_ui(|_| ()).is_some() {
            return Some(Self { owner });
        }
        // comctl32 v6 comes from the application manifest; this registers the classes we ask for
        // by name below. `ICC_TREEVIEW_CLASSES` and `ICC_LISTVIEW_CLASSES` are the two that
        // matter here, and neither is in the standard set.
        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TREEVIEW_CLASSES
                | ICC_LISTVIEW_CLASSES
                | ICC_BAR_CLASSES
                | ICC_STANDARD_CLASSES,
        };
        // SAFETY: `dwSize` declares the struct, which is all the call reads.
        let _ = unsafe { InitCommonControlsEx(&controls) };
        dark_mode::allow_dark_mode_for_app();

        let ui = build(owner, sort_by)?;
        UI.replace(Some(ui));
        // The rows go on now rather than in `build`: putting one on takes and drops the state
        // borrow several times, so the state has to be in the thread-local first.
        tree::populate_roots();
        log::info!("Browse mode's Windows UI built");
        Some(Self { owner })
    }

    /// Show or hide the whole browser. Image mode hides it; browse mode shows it, sized to the
    /// owner's client area first so a resize while hidden can't leave it stale.
    pub fn set_hidden(&self, hidden: bool) {
        let Some(container) = with_ui(|ui| ui.container) else {
            return;
        };
        if !hidden {
            self.relayout();
        }
        // SAFETY: a live window of ours.
        unsafe {
            let _ = ShowWindow(container, if hidden { SW_HIDE } else { SW_SHOW });
        }
    }

    /// Re-fit the container to the owner's client area and lay its children out again. Called on
    /// every window resize and on the way into browse mode.
    pub fn relayout(&self) {
        let mut client = RECT::default();
        // SAFETY: a live window of ours, and the rect is ours to fill.
        if unsafe { GetClientRect(self.owner, &mut client) }.is_err() {
            return;
        }
        let width = client.right - client.left;
        let height = client.bottom - client.top;
        let Some(container) = with_ui(|ui| ui.container) else {
            return;
        };
        // SAFETY: a live window of ours; no activation and no z-order change, so this can't
        // steal focus from the image window.
        unsafe {
            let _ = SetWindowPos(
                container,
                None,
                0,
                0,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        apply_layout(width, height);
    }

    /// Follow the monitor's DPI to a new value: rebuild the font and the metrics, resize the
    /// thumbnails, and lay out again. A `WM_DPICHANGED` reaches winit's window rather than ours,
    /// so `App` passes it on.
    pub fn rescale(&self) {
        let Some(Some(dpi)) = with_ui_mut(Ui::rescale) else {
            return;
        };
        grid::rescale(dpi);
        self.relayout();
    }

    /// Give the keyboard to a pane, and repaint the selection emphasis that follows from it.
    pub fn apply_focus(&self, pane: PaneSide) {
        with_ui_mut(|ui| ui.apply_focus(pane));
    }

    /// The path the grid is sitting on, if any.
    #[must_use]
    pub fn selected_path(&self) -> Option<PathBuf> {
        with_ui(grid::selected_path).flatten()
    }

    /// The grid's selected index, if any.
    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        with_ui(grid::selected_index).flatten()
    }

    /// Every image the grid lists, in display order.
    #[must_use]
    pub fn images(&self) -> Vec<PathBuf> {
        with_ui(grid::images).unwrap_or_default()
    }

    /// How many images the grid lists.
    #[must_use]
    pub fn image_count(&self) -> usize {
        with_ui(grid::image_count).unwrap_or(0)
    }

    /// True when the listed folder has no images, which is what makes the grid non-focusable.
    #[must_use]
    pub fn grid_is_empty(&self) -> bool {
        with_ui(grid::is_empty).unwrap_or(true)
    }

    /// Select a grid item programmatically, the way a click would.
    pub fn select_index(&self, index: usize, scroll: bool) {
        grid::select_index(index, scroll);
    }

    /// Apply a completed folder listing.
    pub fn folder_listed(&self, images: Vec<PathBuf>, preselect: Option<&Path>) {
        grid::folder_listed(images, preselect);
    }

    /// Apply a live folder re-scan to the grid, keeping it on the same file. `true` when the
    /// selected image changed identity, so the caller can re-warm.
    pub fn apply_rescan(&self, images: Vec<PathBuf>, modified: &[PathBuf]) -> bool {
        grid::apply_rescan(images, modified)
    }

    /// Expand the tree from its root down to `folder`, then select it.
    pub fn reveal_folder_in_tree(&self, folder: &Path) {
        tree::reveal(folder);
    }

    /// True while a reveal walk is still in flight.
    #[must_use]
    pub fn reveal_pending(&self) -> bool {
        with_ui(tree::reveal_pending).unwrap_or(false)
    }

    /// Store a finished tree scan and put its children on the row.
    pub fn tree_children_loaded(&self, path: &Path, children: Vec<PathBuf>) {
        tree::children_loaded(path, children);
    }

    /// The tree's root paths, for the live-folder-sync watch.
    #[must_use]
    pub fn tree_root_paths(&self) -> Vec<PathBuf> {
        with_ui(tree::root_paths).unwrap_or_default()
    }

    /// Re-scan a watched tree folder after it changed on disk.
    pub fn reload_tree_node(&self, folder: &Path) {
        tree::invalidate(folder);
    }
}

// ── Building ─────────────────────────────────────────────────────────────────

fn build(owner: HWND, sort_by: crate::navigation::SortBy) -> Option<Ui> {
    register_class();
    // SAFETY: `None` asks for this executable's own module, which always exists.
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    let instance = windows::Win32::Foundation::HINSTANCE(instance.0);

    // No `WS_VISIBLE`: the controls go on before anything is shown, so the browser never appears
    // half-built. `WS_CLIPCHILDREN` keeps the container's own background paint out of the panes.
    // SAFETY: `CLASS_NAME` is registered above and `owner` is winit's live window.
    let container = unsafe {
        CreateWindowExW(
            Default::default(),
            CLASS_NAME,
            PCWSTR::null(),
            WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            Some(owner),
            None,
            Some(instance),
            None,
        )
    }
    .ok()?;

    // SAFETY: a live window of ours. Per-Monitor v2 is declared in the manifest, so this is the
    // DPI of the monitor the window is on.
    let dpi = unsafe { GetDpiForWindow(container) }.max(96);
    let theme = dark_mode::current_theme();
    let font = message_font(dpi);
    dark_mode::apply_to_window(container, theme);

    let status = create_status_bar(container, instance, font, theme)?;
    let status_height = natural_height(status);
    let metrics = Metrics::for_dpi(dpi, status_height);

    let (tree, tree_state) = tree::create(container, instance, font, theme)?;
    let (grid, grid_state) = grid::create(container, instance, font, theme, dpi, sort_by)?;
    // Tab, Enter, and Esc have to be taken off the controls before they handle them, and a
    // subclass is where Win32 does that. Everything else falls through to the control, which is
    // what keeps arrow selection, page keys, and type-select native.
    subclass_pane(tree, ID_TREE);
    subclass_pane(grid, ID_GRID);

    let ui = Ui {
        container,
        tree,
        grid,
        status,
        font,
        theme,
        dpi,
        tree_width: layout::scale(layout::TREE_PANE_DEFAULT, dpi),
        metrics,
        drag: None,
        tree_state,
        grid_state,
        selected_dimensions: None,
    };
    Some(ui)
}

/// Register the container's window class, once per process.
fn register_class() {
    thread_local! {
        static REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if REGISTERED.get() {
        return;
    }
    // SAFETY: `None` asks for this executable's own module.
    let Ok(instance) = (unsafe { GetModuleHandleW(None) }) else {
        return;
    };
    let class = WNDCLASSW {
        // `CS_DBLCLKS` so the grid's double-click reaches us as `NM_DBLCLK` rather than as two
        // separate clicks.
        style: CS_DBLCLKS,
        lpfnWndProc: Some(container_proc),
        hInstance: windows::Win32::Foundation::HINSTANCE(instance.0),
        lpszClassName: CLASS_NAME,
        // No class background brush: `WM_ERASEBKGND` paints the theme's own colour, which a
        // class brush would fight over on every dark-mode switch.
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        hCursor: HCURSOR(std::ptr::null_mut()),
        hIcon: HICON(std::ptr::null_mut()),
        cbClsExtra: 0,
        cbWndExtra: 0,
        lpszMenuName: PCWSTR::null(),
    };
    // SAFETY: every pointer in the struct is either null or `'static`.
    if unsafe { RegisterClassW(&class) } == 0 {
        log::error!("Couldn't register the browse container's window class");
        return;
    }
    REGISTERED.set(true);
}

fn create_status_bar(
    parent: HWND,
    instance: windows::Win32::Foundation::HINSTANCE,
    font: HFONT,
    theme: Theme,
) -> Option<HWND> {
    // No `SBARS_SIZEGRIP`: the grip belongs to a resizable top-level window, and this is a child
    // of one whose own frame already has it.
    // SAFETY: `STATUSCLASSNAMEW` is registered by `InitCommonControlsEx` with `ICC_BAR_CLASSES`.
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            STATUSCLASSNAMEW,
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(ID_STATUS as isize as *mut std::ffi::c_void)),
            Some(instance),
            None,
        )
    }
    .ok()?;
    set_font(hwnd, font);
    dark_mode::apply_to_window(hwnd, theme);
    Some(hwnd)
}

/// A control's height as the system sized it. A status bar measures itself from the message
/// font, so it is read rather than computed.
fn natural_height(hwnd: HWND) -> i32 {
    let mut rect = RECT::default();
    // SAFETY: a live control of ours, and the rect is ours to fill.
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return 0;
    }
    rect.bottom - rect.top
}

/// The system's message font at this DPI. The same call the settings dialog makes, and for the
/// same reason: a control left with the stock font looks like a Windows 3.1 app.
fn message_font(dpi: u32) -> HFONT {
    use windows::Win32::Graphics::Gdi::CreateFontIndirectW;

    let mut metrics = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `cbSize` declares the buffer and the pointer is to that same struct. The `ForDpi`
    // form returns a height already scaled for the monitor.
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
        return HFONT(std::ptr::null_mut());
    }
    // SAFETY: the `LOGFONTW` came from the system a line ago.
    unsafe { CreateFontIndirectW(&metrics.lfMessageFont) }
}

pub(super) fn set_font(hwnd: HWND, font: HFONT) {
    if font.0.is_null() {
        return;
    }
    // SAFETY: a live control of ours and a font that outlives it. `lparam` of 1 asks for a
    // repaint, which costs nothing before the window is shown.
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        )
    };
}

/// A Rust string as a NUL-terminated UTF-16 buffer, for the Win32 `W` calls.
pub(super) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// ── The panes' three keys ────────────────────────────────────────────────────

/// Put [`pane_proc`] in front of a control's own window procedure.
fn subclass_pane(hwnd: HWND, id: i32) {
    // SAFETY: a live control of ours, a `'static` procedure, and an id unique among this
    // window's subclasses. The subclass is removed in `WM_NCDESTROY`.
    let installed = unsafe { SetWindowSubclass(hwnd, Some(pane_proc), id as usize, 0) };
    if !installed.as_bool() {
        log::error!("Couldn't subclass a browse pane; Tab, Enter, and Esc won't work in it");
    }
}

/// The three keys a browse pane routes as commands, and Backspace in the tree.
///
/// Everything else reaches the control, which is the whole point: `SysTreeView32` and
/// `SysListView32` already do arrow selection, page keys, Home, End, and type-select better than
/// we would, and taking any of them would replace a native behaviour with a worse one.
unsafe extern "system" fn pane_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _reference: usize,
) -> LRESULT {
    match message {
        WM_KEYDOWN => {
            let virtual_key = wparam.0 as u32;
            // Explorer's convention, and free here: in browse mode Backspace has no other job,
            // where in image mode it means Previous.
            if id == ID_TREE as usize && virtual_key == keys::vk::BACK {
                tree::select_parent();
                return LRESULT(0);
            }
            if let Some(command) = keys::browse_keydown_command(virtual_key) {
                crate::commands::send_command(command);
                return LRESULT(0);
            }
        }
        WM_NCDESTROY => {
            // SAFETY: the subclass this procedure was installed as, on its own window.
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(pane_proc), id) };
        }
        _ => {}
    }
    // SAFETY: forwarding a message we didn't handle to the control's own procedure.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

// ── Layout, focus, and the status bar ────────────────────────────────────────

/// Put the three panes and the status bar where [`super::layout`] says they go.
///
/// The status bar re-measures itself first: Windows sizes one from the system font, so its height
/// is read rather than computed. `SetWindowPos` on a pane sends it a `WM_SIZE`, so no state borrow
/// is held while the children are placed.
fn apply_layout(client_width: i32, client_height: i32) {
    let Some((tree, grid, status, placed, dpi)) = with_ui_mut(|ui| {
        ui.metrics.status_bar = natural_height(ui.status);
        ui.tree_width = layout::clamp_tree_width(ui.tree_width, client_width, ui.metrics);
        let placed = layout::layout(client_width, client_height, ui.tree_width, ui.metrics);
        (ui.tree, ui.grid, ui.status, placed, ui.dpi)
    }) else {
        return;
    };
    place(tree, placed.tree);
    place(grid, placed.grid);
    place(status, placed.status_bar);
    apply_status_parts(status, placed.status_bar.width, dpi);
    // The grid's visible range moved, so more or fewer thumbnails are worth generating.
    grid::pump_visible_range();
}

/// Split the status bar into its three panes. The count and size panes are fixed; the name pane
/// takes what's left, because a file name has no bound on its length.
fn apply_status_parts(status: HWND, width: i32, dpi: u32) {
    let size_width = layout::scale(STATUS_SIZE_WIDTH, dpi);
    let count_width = layout::scale(STATUS_COUNT_WIDTH, dpi);
    let edges: [i32; STATUS_PARTS] = [count_width, (width - size_width).max(count_width), -1];
    // SAFETY: a live status bar of ours; `wparam` declares how many edges the pointer holds.
    unsafe {
        SendMessageW(
            status,
            SB_SETPARTS,
            Some(WPARAM(STATUS_PARTS)),
            Some(LPARAM(edges.as_ptr() as isize)),
        )
    };
}

impl Ui {
    /// What the status bar should say, read from the model. The `SB_SETTEXT` calls happen in
    /// [`refresh_status_bar`], with no borrow held.
    fn status_fields(&self) -> status::Fields {
        let selected = grid::selected_path(self);
        status::fields(status::Status {
            image_count: grid::image_count(self),
            selected: selected.as_deref(),
            dimensions: self.selected_dimensions,
            loading: tree::scan_pending(self) || grid::listing_pending(self),
        })
    }

    /// Hand the keyboard to a pane and repaint what follows from it. `SetFocus` is all the
    /// emphasis a treeview needs; the grid draws its selection greyed while unfocused, which is
    /// also automatic once focus moves.
    fn apply_focus(&mut self, pane: PaneSide) {
        let target = match pane {
            PaneSide::Tree => self.tree,
            PaneSide::Grid => self.grid,
        };
        // SAFETY: a live control of ours. A failure here means another window took focus first,
        // which is not ours to fight over.
        let _ = unsafe { SetFocus(Some(target)) };
    }

    /// Rebuild everything the monitor's DPI decides. The image list is rebuilt by `grid`, which
    /// takes its own borrow, so this returns the new DPI rather than doing it here.
    fn rescale(&mut self) -> Option<u32> {
        // SAFETY: a live window of ours.
        let dpi = unsafe { GetDpiForWindow(self.container) }.max(96);
        if dpi == self.dpi {
            return None;
        }
        let logical_tree_width = self.tree_width * 96 / self.dpi.max(1) as i32;
        self.dpi = dpi;
        let old_font = self.font;
        self.font = message_font(dpi);
        for control in [self.tree, self.grid, self.status] {
            set_font(control, self.font);
        }
        delete_font(old_font);
        self.metrics = Metrics::for_dpi(dpi, natural_height(self.status));
        self.tree_width = layout::scale(logical_tree_width, dpi);
        log::debug!("Browse mode rescaled to {dpi} DPI");
        Some(dpi)
    }
}

fn delete_font(font: HFONT) {
    if font.0.is_null() {
        return;
    }
    use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
    // SAFETY: a font this module created and nothing holds any more.
    let _ = unsafe { DeleteObject(HGDIOBJ(font.0)) };
}

fn place(hwnd: HWND, rect: Rect) {
    // SAFETY: a live control of ours; no activation and no z-order change.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

// ── The window procedure ─────────────────────────────────────────────────────

unsafe extern "system" fn container_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SIZE => {
            // The status bar re-aligns itself to the parent's bottom when told its parent
            // resized, which is how it stays the height the system font asks for.
            if let Some(status) = with_ui(|ui| ui.status) {
                // SAFETY: a live status bar of ours.
                unsafe { SendMessageW(status, WM_SIZE, Some(WPARAM(0)), Some(LPARAM(0))) };
            }
            let width = (lparam.0 & 0xffff) as i16 as i32;
            let height = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            apply_layout(width, height);
            LRESULT(0)
        }

        WM_ERASEBKGND => {
            // The gap the splitter lives in is the only pixel of the container a pane doesn't
            // cover, so this is what draws the splitter: the window background, in the theme's
            // own colour.
            let Some(theme) = with_ui(|ui| ui.theme) else {
                return LRESULT(0);
            };
            let mut client = RECT::default();
            // SAFETY: a live window of ours, and the rect is ours to fill.
            if unsafe { GetClientRect(hwnd, &mut client) }.is_ok() {
                // SAFETY: `wparam` is the device context Windows handed us, and the brush is
                // owned by `dark_mode` for the life of the process.
                unsafe {
                    FillRect(
                        HDC(wparam.0 as *mut _),
                        &client,
                        dark_mode::background_brush(theme),
                    )
                };
            }
            LRESULT(1)
        }

        WM_CTLCOLORSTATIC => {
            // The status bar's panes are statics, and in dark mode they'd otherwise paint black
            // text on the theme's grey.
            let Some(theme) = with_ui(|ui| ui.theme) else {
                return LRESULT(0);
            };
            let (background, text) = theme.colors();
            let hdc = HDC(wparam.0 as *mut _);
            // SAFETY: the device context is the one Windows is asking us to prepare.
            unsafe {
                SetBkColor(hdc, background);
                SetTextColor(hdc, text);
            }
            LRESULT(dark_mode::background_brush(theme).0 as isize)
        }

        WM_NOTIFY => {
            // SAFETY: for `WM_NOTIFY`, `lparam` is an `NMHDR` Windows owns for the call. It stays
            // a pointer: the listview's `LVN_GETDISPINFO` writes back through it.
            let header = lparam.0 as *mut NMHDR;
            let from = unsafe { (*header).idFrom } as i32;
            match from {
                ID_TREE => tree::notify(header),
                ID_GRID => grid::notify(header),
                _ => LRESULT(0),
            }
        }

        WM_SETCURSOR | WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
            match splitter(hwnd, message, lparam) {
                Some(result) => result,
                // SAFETY: forwarding a message we didn't handle, with its own arguments.
                None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
            }
        }

        WM_DESTROY => {
            // The children go with the parent, so all that is left is the font and the models.
            if let Some(font) = with_ui(|ui| ui.font) {
                delete_font(font);
            }
            UI.replace(None);
            log::debug!("Browse mode's Windows UI destroyed");
            LRESULT(0)
        }

        // SAFETY: forwarding a message we don't handle, with its own arguments.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

// ── The splitter ─────────────────────────────────────────────────────────────

/// The splitter's share of the mouse. `None` means the message wasn't the splitter's, and the
/// caller passes it to `DefWindowProc`.
///
/// Win32 has no splitter control. The gap between the panes is the container's own client area,
/// so the messages arrive here with no fourth window to own them, and the drag is `SetCapture`
/// plus a clamp that [`super::layout`] already tests.
fn splitter(hwnd: HWND, message: u32, lparam: LPARAM) -> Option<LRESULT> {
    let dragging = with_ui(|ui| ui.drag.is_some())?;
    match message {
        WM_SETCURSOR => {
            // The gap is the only part of the container the panes leave uncovered, so a cursor
            // query that reaches us at all is over the splitter.
            if !dragging && !cursor_over_splitter(hwnd)? {
                return None;
            }
            // SAFETY: a stock cursor, which is always loadable.
            unsafe {
                if let Ok(cursor) = LoadCursorW(None, IDC_SIZEWE) {
                    SetCursor(Some(cursor));
                }
            }
            Some(LRESULT(1))
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let grab = with_ui_mut(|ui| x - ui.tree_width)?;
            with_ui_mut(|ui| ui.drag = Some(grab));
            // SAFETY: a live window of ours. Capture is released on the button up below.
            unsafe { SetCapture(hwnd) };
            Some(LRESULT(0))
        }
        WM_MOUSEMOVE if dragging => {
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let mut client = RECT::default();
            // SAFETY: a live window of ours.
            if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
                return Some(LRESULT(0));
            }
            let width = client.right - client.left;
            let height = client.bottom - client.top;
            with_ui_mut(|ui| {
                let grab = ui.drag.unwrap_or(0);
                ui.tree_width = layout::tree_width_for_drag(x, grab, width, ui.metrics);
            });
            apply_layout(width, height);
            // SAFETY: a live window of ours; the gap has to repaint where the splitter left.
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            Some(LRESULT(0))
        }
        WM_LBUTTONUP if dragging => {
            with_ui_mut(|ui| ui.drag = None);
            // SAFETY: we took capture on the matching button down.
            let _ = unsafe { ReleaseCapture() };
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// Whether the pointer is in the gap between the two panes right now.
fn cursor_over_splitter(hwnd: HWND) -> Option<bool> {
    let mut point = POINT::default();
    // SAFETY: the point is ours to fill, and the window is ours.
    unsafe {
        GetCursorPos(&mut point).ok()?;
        let _ = ScreenToClient(hwnd, &mut point);
    }
    with_ui(|ui| point.x >= ui.tree_width && point.x < ui.tree_width + ui.metrics.splitter)
}

// ── What the rest of browse mode calls ───────────────────────────────────────

/// Colour a `COLORREF` the way `dark_mode` means it, for the modules next door.
pub(super) fn theme_colors(theme: Theme) -> (COLORREF, COLORREF) {
    theme.colors()
}

/// Start listing a folder's images for the grid, on the background worker.
pub fn list_folder(folder: PathBuf) {
    grid::list_folder(folder);
}

/// Apply every queued thumbnail into the grid's image list.
pub fn thumbnails_available() {
    grid::thumbnails_available();
}

/// Rewrite the status bar. Called whenever anything it reports changes, and the one place its
/// text is set.
pub fn refresh_status_bar() {
    let Some((status, fields)) = with_ui(|ui| (ui.status, ui.status_fields())) else {
        return;
    };
    for (part, text) in [
        (0usize, &fields.count),
        (1, &fields.name),
        (2, &fields.size),
    ] {
        let text = wide(text);
        // SAFETY: a live status bar of ours, and the string outlives the call. The part index
        // carries no drawing flags, so the pane gets the sunken border a status bar draws by
        // default.
        unsafe {
            SendMessageW(
                status,
                SB_SETTEXTW,
                Some(WPARAM(part)),
                Some(LPARAM(text.as_ptr() as isize)),
            )
        };
    }
}

/// Record a finished header read for the selected image, for the status bar's size pane. A
/// delivery for a file that is no longer selected is dropped: arrowing through a folder fires a
/// measurement per cell, and a late answer would show the wrong size beside the right name.
pub fn selection_measured(path: &Path, dimensions: Option<(u32, u32)>) {
    let matched = with_ui_mut(|ui| {
        let current = grid::selected_path(ui);
        let matched = current
            .as_deref()
            .is_some_and(|current| crate::paths::same_path(current, path));
        if matched {
            ui.selected_dimensions = dimensions;
        }
        matched
    });
    if matched == Some(true) {
        refresh_status_bar();
    }
}
