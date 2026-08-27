//! The thumbnail grid: a virtual `SysListView32` in icon mode, themed as Explorer's.
//!
//! `LVS_OWNERDATA` is what makes it virtual: the listview holds a count rather than items, and
//! asks us for a cell's text and image as it draws. A 5,000-image folder therefore costs a
//! `LVM_SETITEMCOUNT` and nothing else, where pushing items in would cost 5,000 allocations
//! before the first pixel.
//!
//! Everything below the widget is the same machinery the macOS grid drives: [`GridModel`] for
//! the folder listing and the selection, [`Scheduler`] for what to generate next,
//! [`ThumbnailCache`] for what to drop, and `previews::generator` for the pixels — which on
//! Windows reads Explorer's own `thumbcache_*.db` through `IShellItemImageFactory`, so a folder
//! Explorer has already visited paints from cache rather than from a decode.
//!
//! ## Decision: a fixed pool of image list slots, recycled by hand
//!
//! **Why:** an `HIMAGELIST` has no way to remove an image without renumbering every image after
//! it, and the grid's whole eviction story is dropping thumbnails that scrolled away. So the
//! list is created at a fixed count, a slot is claimed when a thumbnail lands and returned when
//! it is evicted, and `ImageList_Replace` writes into a slot that keeps its number for the
//! folder's life. The byte budget is set from the same count, so eviction never has to fail for
//! want of a slot.
//!
//! ## Gotcha: never hold the state borrow across a listview message
//!
//! **Why:** `LVM_SETITEMCOUNT` can send `LVN_ODCACHEHINT` and `LVM_SETITEMSTATE` sends
//! `LVN_ITEMCHANGED`, both **synchronously** to the parent — which lands back in [`notify`] and
//! borrows the same `RefCell` this call was holding, and breaking that rule aborts the process
//! rather than misbehaving (`super`, and `browser/windows/CLAUDE.md`). So the model work happens
//! under the borrow, the borrow is dropped, and the messages go out afterwards. `super::tree`
//! keeps the same rule for the same reason. The image list calls are the exception and are safe
//! under a borrow: `CreateDIBSection`, `ImageList_Replace`, and `DeleteObject` operate on the
//! image list object and reach no window procedure.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HDC,
    HGDIOBJ,
};
use windows::Win32::UI::Controls::{
    HIMAGELIST, ILC_COLOR32, ImageList_Create, ImageList_Destroy, ImageList_Replace,
    ImageList_SetImageCount, LVIF_IMAGE, LVIF_STATE, LVIF_TEXT, LVIS_SELECTED, LVITEMW,
    LVN_GETDISPINFOW, LVN_ITEMCHANGED, LVN_ODCACHEHINT, LVS_AUTOARRANGE, LVS_ICON, LVS_OWNERDATA,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, LVSIL_NORMAL, NM_DBLCLK, NMHDR, NMLISTVIEW, NMLVCACHEHINT,
    NMLVDISPINFOW, WC_LISTVIEWW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, HMENU, SendMessageW, WS_BORDER, WS_CHILD, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::PCWSTR;

use crate::browser::grid_listing::FolderLister;
use crate::browser::grid_model::{self, GridModel};
use crate::browser::grid_scheduler::Scheduler;
use crate::browser::thumbnail_cache::ThumbnailCache;
use crate::navigation::SortBy;
use crate::paths::PathPolicy;
use crate::previews::generator::RequestTable;
use crate::previews::request::SubmitRequest;

use super::super::selection_meta::SelectionMeasurer;
use super::super::{layout, thumbnail};
use super::{ID_GRID, Ui, set_font, wide, with_ui, with_ui_mut};

/// How many thumbnails the image list holds. Beyond this the cache evicts, which is what stops a
/// 5,000-image folder from turning into a gigabyte of bitmaps. At the default 128-pixel slot
/// that is 33 MB, comfortably inside the 128 MB the macOS grid budgets for the same job.
const SLOTS: usize = 512;

/// Listview messages, from `commctrl.h`. `LVM_FIRST` is `0x1000`.
const LVM_SETIMAGELIST: u32 = 0x1000 + 3;
const LVM_SETITEMCOUNT: u32 = 0x1000 + 47;
const LVM_REDRAWITEMS: u32 = 0x1000 + 21;
const LVM_SETITEMSTATE: u32 = 0x1000 + 43;
const LVM_ENSUREVISIBLE: u32 = 0x1000 + 19;
const LVM_SETICONSPACING: u32 = 0x1000 + 53;
const LVM_GETORIGIN: u32 = 0x1000 + 41;
const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1000 + 54;
const LVS_EX_DOUBLEBUFFER: u32 = 0x0001_0000;

/// Everything the grid knows that isn't a window handle.
pub(super) struct GridState {
    /// The folder's images, the sort order, the selection, and the folder generation.
    model: GridModel,
    /// What to generate next, centred on the visible range.
    scheduler: Scheduler,
    /// What to drop, by byte budget.
    cache: ThumbnailCache,
    /// The image list every cell draws from, and the slot each generated thumbnail sits in.
    images: HIMAGELIST,
    /// Folder index to image list slot, for the thumbnails currently resident.
    slots: HashMap<usize, i32>,
    /// Slots nothing is using. Claimed on arrival, returned on eviction.
    free_slots: Vec<i32>,
    /// One slot's side in device pixels, which is also what a thumbnail is generated at.
    slot_side: u32,
    /// The background workers: listing a folder, generating thumbnails, and measuring the
    /// selected image for the status bar.
    lister: FolderLister,
    requests: RequestTable,
    measurer: SelectionMeasurer,
    /// True between asking for a folder listing and its arrival, so the status bar can say
    /// "Loading…" rather than the previous folder's count.
    listing: bool,
    /// The cell text the listview is currently asking about. A virtual listview wants a pointer
    /// that outlives the notification, so the string it is handed lives here.
    label: Vec<u16>,
}

/// Build the listview and its state.
pub(super) fn create(
    parent: HWND,
    instance: HINSTANCE,
    font: windows::Win32::Graphics::Gdi::HFONT,
    theme: crate::chrome::Theme,
    dpi: u32,
    sort_by: SortBy,
) -> Option<(HWND, GridState)> {
    let style = WS_CHILD
        | WS_VISIBLE
        | WS_TABSTOP
        | WS_BORDER
        | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
            LVS_ICON | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_OWNERDATA | LVS_AUTOARRANGE,
        );
    // SAFETY: `WC_LISTVIEWW` is registered by `InitCommonControlsEx` with
    // `ICC_LISTVIEW_CLASSES`, and `parent` is a live window of ours.
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            WC_LISTVIEWW,
            PCWSTR::null(),
            style,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(ID_GRID as isize as *mut std::ffi::c_void)),
            Some(instance),
            None,
        )
    }
    .ok()?;

    set_font(hwnd, font);
    crate::platform::windows::dark_mode::apply_to_window(hwnd, theme);
    // SAFETY: a live listview of ours. Double buffering is what stops a scroll flickering.
    unsafe {
        SendMessageW(
            hwnd,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM(LVS_EX_DOUBLEBUFFER as usize)),
            Some(LPARAM(LVS_EX_DOUBLEBUFFER as isize)),
        )
    };

    let slot_side = layout::scale(layout::CELL_THUMBNAIL, dpi).max(1) as u32;
    let images = create_image_list(slot_side)?;
    // SAFETY: a live listview of ours, and an image list this module owns for its lifetime.
    unsafe {
        SendMessageW(
            hwnd,
            LVM_SETIMAGELIST,
            Some(WPARAM(LVSIL_NORMAL as usize)),
            Some(LPARAM(images.0)),
        )
    };
    apply_icon_spacing(hwnd, dpi);

    let state = GridState {
        model: GridModel::new(sort_by),
        // Half the cores, floor 1 — the same courtesy cap the macOS grid and the preview
        // generator both take.
        scheduler: Scheduler::new(crate::previews::max_parallel()),
        cache: ThumbnailCache::with_budget(SLOTS * thumbnail::slot_bytes(slot_side)),
        images,
        slots: HashMap::new(),
        free_slots: (0..SLOTS as i32).rev().collect(),
        slot_side,
        lister: FolderLister::start(),
        requests: RequestTable::new(
            || crate::commands::AppCommand::BrowseThumbnailsAvailable,
            "prvw-gridgen",
        ),
        measurer: SelectionMeasurer::start(),
        listing: false,
        label: Vec::new(),
    };
    Some((hwnd, state))
}

fn create_image_list(side: u32) -> Option<HIMAGELIST> {
    // SAFETY: a size and a count; the call allocates and answers null on failure.
    let images =
        unsafe { ImageList_Create(side as i32, side as i32, ILC_COLOR32, SLOTS as i32, 0) };
    if images.is_invalid() {
        log::error!("Couldn't create the browse grid's image list");
        return None;
    }
    // Every slot exists from the start, so `ImageList_Replace` can write into one without the
    // renumbering that adding and removing would cause.
    // SAFETY: an image list this module owns.
    if !unsafe { ImageList_SetImageCount(images, SLOTS as u32) }.as_bool() {
        // SAFETY: a list nothing else refers to yet.
        let _ = unsafe { ImageList_Destroy(Some(images)) };
        return None;
    }
    Some(images)
}

/// Tell the listview how big a cell is, so its own arithmetic and
/// [`layout::visible_range`] agree about which item is where.
fn apply_icon_spacing(hwnd: HWND, dpi: u32) {
    let (width, height) = layout::cell_size(dpi);
    // SAFETY: a live listview of ours; the two sizes are packed into `lparam` as the message
    // documents.
    unsafe {
        SendMessageW(
            hwnd,
            LVM_SETICONSPACING,
            None,
            Some(LPARAM(((height << 16) | (width & 0xffff)) as isize)),
        )
    };
}

// ── What the rest of browse mode calls ───────────────────────────────────────

/// How many images the listed folder holds.
pub(super) fn image_count(ui: &Ui) -> usize {
    ui.grid_state.model.len()
}

/// True between asking for a folder listing and its arrival.
pub(super) fn listing_pending(ui: &Ui) -> bool {
    ui.grid_state.listing
}

/// The selected image's path, if any.
pub(super) fn selected_path(ui: &Ui) -> Option<PathBuf> {
    ui.grid_state.model.selected_path().map(Path::to_path_buf)
}

/// The selected index, if any.
pub(super) fn selected_index(ui: &Ui) -> Option<usize> {
    ui.grid_state.model.selected()
}

/// Every image in the folder, in display order.
pub(super) fn images(ui: &Ui) -> Vec<PathBuf> {
    ui.grid_state.model.images().to_vec()
}

/// True when the folder has no images, which is what makes the grid non-focusable.
pub(super) fn is_empty(ui: &Ui) -> bool {
    ui.grid_state.model.is_empty()
}

/// Start listing a folder's images on the background worker. Never reads the disk here: a slow
/// folder selection must not freeze the UI.
pub(super) fn list_folder(folder: PathBuf) {
    with_ui_mut(|ui| {
        ui.grid_state.listing = true;
        ui.grid_state.lister.list(folder);
    });
    super::refresh_status_bar();
}

/// Apply a finished listing: replace the model's images, reset the listview's count, drop every
/// thumbnail of the folder we came from, and preselect either the image we came from or the
/// first one.
pub(super) fn folder_listed(images: Vec<PathBuf>, preselect: Option<&Path>) {
    let Some((grid, count, preselect_index)) = with_ui_mut(|ui| {
        ui.grid_state.listing = false;
        // Abandon the previous folder's queued generation, and free every slot it held.
        ui.grid_state.requests.cancel_all();
        release_all_slots(&mut ui.grid_state);
        // `set_images` sorts, so the preselect is resolved against the model's own order
        // afterwards — which is what makes the grid's index and the image-mode `DirectoryList`
        // index the same number (`browser::resolve_reveal_index`).
        ui.grid_state.model.set_images(images);
        let count = ui.grid_state.model.len();
        let preselect_index =
            crate::browser::grid_preselect_index(ui.grid_state.model.images(), preselect)
                .unwrap_or(0);
        ui.grid_state
            .scheduler
            .set_folder(count, grid_model::clamp_visible_range(0..0, count));
        ui.grid_state.cache =
            ThumbnailCache::with_budget(SLOTS * thumbnail::slot_bytes(ui.grid_state.slot_side));
        (ui.grid, count, preselect_index)
    }) else {
        return;
    };

    // No borrow from here on: both messages notify the parent (see the module's gotcha).
    set_item_count(grid, count);
    if count > 0 {
        select_index(preselect_index, true);
    }
    pump_visible_range();
    super::refresh_status_bar();
}

/// Apply a live folder re-scan: replace the listed images keeping the grid on the same **file**
/// rather than the same index, and tell the caller whether the selected image is now a different
/// one (so it can re-warm).
pub(super) fn apply_rescan(images: Vec<PathBuf>, modified: &[PathBuf]) -> bool {
    let Some((grid, count, selected, changed)) = with_ui_mut(|ui| {
        let before = ui.grid_state.model.selected_path().map(Path::to_path_buf);
        // Every slot is keyed by folder index, and a re-scan renumbers them. Rather than track
        // which file moved where, drop them all and let the visible range regenerate: that is
        // one screen of thumbnails, and it is exactly what a shell-cache read is cheap for.
        release_all_slots(&mut ui.grid_state);
        ui.grid_state.model.apply_rescan(images, before.as_deref());
        let count = ui.grid_state.model.len();
        ui.grid_state
            .scheduler
            .set_folder(count, grid_model::clamp_visible_range(0..0, count));
        // A file whose bytes changed has a stale thumbnail in Explorer's cache too, so the
        // generator is asked again for it rather than trusting either cache.
        if !modified.is_empty() {
            ui.grid_state.requests.cancel_all();
        }
        let after = ui.grid_state.model.selected_path().map(Path::to_path_buf);
        (
            ui.grid,
            count,
            ui.grid_state.model.selected(),
            before != after,
        )
    }) else {
        return false;
    };

    set_item_count(grid, count);
    // Re-assert the native selection: the file the model now points at may sit at a different
    // index than the listview's own cursor.
    if let Some(index) = selected {
        select_index(index, false);
    }
    pump_visible_range();
    super::refresh_status_bar();
    changed
}

fn set_item_count(hwnd: HWND, count: usize) {
    // SAFETY: a live listview of ours.
    unsafe { SendMessageW(hwnd, LVM_SETITEMCOUNT, Some(WPARAM(count)), Some(LPARAM(0))) };
}

/// Select `index`, optionally scrolling it into view, and keep the model in step.
///
/// `LVM_SETITEMSTATE` notifies the parent synchronously, so the model update and the message are
/// deliberately separate steps (see the module's re-entrancy gotcha).
pub(super) fn select_index(index: usize, scroll: bool) {
    let Some(Some((grid, resolved, path))) = with_ui_mut(|ui| {
        let resolved = ui.grid_state.model.set_selected(index)?;
        // Clear the old measurement now, so the size pane goes empty rather than showing the
        // previous file's size beside the new file's name.
        ui.selected_dimensions = None;
        let path = ui.grid_state.model.selected_path().map(Path::to_path_buf);
        Some((ui.grid, resolved, path))
    }) else {
        return;
    };

    let mut state = LVITEMW {
        state: LVIS_SELECTED,
        stateMask: LVIS_SELECTED,
        ..Default::default()
    };
    // `LVIS_FOCUSED` is 1, and a virtual listview needs it as well as the selection or the arrow
    // keys have no anchor to move from.
    state.state.0 |= 1;
    state.stateMask.0 |= 1;
    // SAFETY: a live listview of ours; the struct outlives the call. It notifies the parent, so
    // no borrow is held.
    unsafe {
        SendMessageW(
            grid,
            LVM_SETITEMSTATE,
            Some(WPARAM(resolved)),
            Some(LPARAM(std::ptr::from_mut(&mut state) as isize)),
        )
    };
    if scroll {
        // SAFETY: same listview; `lparam` of 0 means "scroll it fully into view".
        unsafe {
            SendMessageW(
                grid,
                LVM_ENSUREVISIBLE,
                Some(WPARAM(resolved)),
                Some(LPARAM(0)),
            )
        };
    }
    if let Some(path) = path {
        with_ui(|ui| ui.grid_state.measurer.measure(path));
    }
}

/// Recompute the visible range from the listview's scroll position and pump generation.
///
/// The geometry is read before the borrow is taken, so nothing here can re-enter.
pub(super) fn pump_visible_range() {
    let Some((grid, dpi, len)) = with_ui(|ui| (ui.grid, ui.dpi, ui.grid_state.model.len())) else {
        return;
    };
    let range = current_visible_range(grid, dpi, len);
    with_ui_mut(|ui| {
        ui.grid_state.scheduler.set_visible_range(range.clone());
        ui.grid_state.cache.set_visible_range(range);
        let evicted = ui.grid_state.cache.evict_to_budget();
        release_slots(&mut ui.grid_state, &evicted);
        pump(ui);
    });
}

/// Where the listview has scrolled to, turned into folder indices by [`layout::visible_range`].
fn current_visible_range(grid: HWND, dpi: u32, len: usize) -> std::ops::Range<usize> {
    let mut origin = POINT::default();
    // SAFETY: a live listview of ours, and the point is ours to fill. `LVM_GETORIGIN` answers 0
    // in a view that has no scroll origin, which is the same as "not scrolled".
    let read = unsafe {
        SendMessageW(
            grid,
            LVM_GETORIGIN,
            None,
            Some(LPARAM(std::ptr::from_mut(&mut origin) as isize)),
        )
    };
    let scroll_y = if read.0 == 0 { 0 } else { origin.y };
    let mut client = windows::Win32::Foundation::RECT::default();
    // SAFETY: a live control of ours.
    if unsafe { windows::Win32::UI::WindowsAndMessaging::GetClientRect(grid, &mut client) }.is_err()
    {
        return 0..0;
    }
    layout::visible_range(
        scroll_y,
        client.right - client.left,
        client.bottom - client.top,
        layout::cell_size(dpi),
        len,
    )
}

/// Drain the scheduler into the generator pool, stamped with the folder generation so a
/// completion for a folder the user has left is dropped rather than drawn.
fn pump(ui: &mut Ui) {
    let generation = ui.grid_state.model.generation();
    let side = f64::from(ui.grid_state.slot_side);
    loop {
        let Some((index, request_id)) = ui.grid_state.scheduler.poll_next() else {
            return;
        };
        let Some(path) = ui.grid_state.model.path(index).map(Path::to_path_buf) else {
            ui.grid_state.scheduler.mark_failed(index);
            continue;
        };
        ui.grid_state.requests.submit(SubmitRequest {
            request_id,
            index,
            folder_generation: generation,
            path: &path,
            // The slot side is already in device pixels, so the scale is 1: asking for points
            // and a scale factor would apply the monitor's DPI twice.
            size_pt: side,
            scale: 1.0,
            proxy: crate::commands::event_loop_proxy(),
        });
    }
}

/// Apply every queued thumbnail: drop the stale ones, compose the rest into an image list slot,
/// and redraw the cells that changed.
pub(super) fn thumbnails_available() {
    let redraw = with_ui_mut(|ui| {
        let batch = ui.grid_state.requests.drain_pending();
        let generation = ui.grid_state.model.generation();
        let side = ui.grid_state.slot_side;
        let background = pane_background(ui);
        let mut ready = Vec::new();
        for delivery in batch {
            if delivery.folder_generation != generation {
                // A folder the user has left. Tell the scheduler so the slot isn't stuck "in
                // flight" forever.
                ui.grid_state.scheduler.mark_failed(delivery.index);
                continue;
            }
            let Ok(pixels) = delivery.result else {
                ui.grid_state.scheduler.mark_failed(delivery.index);
                continue;
            };
            let Some(slot) = claim_slot(&mut ui.grid_state, delivery.index) else {
                ui.grid_state.scheduler.mark_failed(delivery.index);
                continue;
            };
            let canvas = thumbnail::compose_slot(
                &pixels.rgba,
                pixels.width,
                pixels.height,
                side,
                background,
            );
            if write_slot(ui.grid_state.images, slot, side, &canvas) {
                ui.grid_state.scheduler.mark_ready(delivery.index);
                ui.grid_state
                    .cache
                    .insert(delivery.index, thumbnail::slot_bytes(side));
                ready.push(delivery.index);
            } else {
                release_slots(&mut ui.grid_state, &[delivery.index]);
                ui.grid_state.scheduler.mark_failed(delivery.index);
            }
        }
        // The inserts may have pushed the cache over budget.
        let evicted = ui.grid_state.cache.evict_to_budget();
        release_slots(&mut ui.grid_state, &evicted);
        ready.retain(|index| ui.grid_state.slots.contains_key(index));
        pump(ui);
        ready
    });
    if let Some(ready) = redraw
        && !ready.is_empty()
        && let (Some(first), Some(last)) =
            (ready.iter().min().copied(), ready.iter().max().copied())
    {
        // The handle comes out from under the borrow first, the way every other message in
        // this module does: `LVM_REDRAWITEMS` only invalidates today, but a message sent while
        // the state is borrowed is one comctl32 change away from a panic.
        if let Some(grid) = with_ui(|ui| ui.grid) {
            // SAFETY: a live listview of ours; the two indices bound the rows to repaint.
            unsafe {
                SendMessageW(
                    grid,
                    LVM_REDRAWITEMS,
                    Some(WPARAM(first)),
                    Some(LPARAM(last as isize)),
                )
            };
        }
    }
}

/// The colour a thumbnail is letterboxed against: the **listview's** own background, so a
/// portrait photo doesn't sit in a visible box on the grid.
///
/// Deliberately not `Theme::colors`, which is the dialog background a settings window paints on. A
/// listview paints `COLOR_WINDOW` in light mode — asked for rather than named, so a high-contrast
/// scheme's own colour comes back — and comctl32's `DarkMode_Explorer` list background in dark,
/// which is Windows 11's near-black rather than the dialog grey.
fn pane_background(ui: &Ui) -> (u8, u8, u8) {
    /// comctl32's dark list background, `0x00BBGGRR`.
    const DARK_LIST_BACKGROUND: u32 = 0x0019_1919;

    let value = match ui.theme {
        crate::chrome::Theme::Dark => DARK_LIST_BACKGROUND,
        // SAFETY: a constant index, and the call has no failure mode.
        crate::chrome::Theme::Light => unsafe {
            windows::Win32::Graphics::Gdi::GetSysColor(windows::Win32::Graphics::Gdi::COLOR_WINDOW)
        },
    };
    (
        (value & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        ((value >> 16) & 0xff) as u8,
    )
}

/// Take a free image list slot for `index`, or `None` when every one is in use.
fn claim_slot(state: &mut GridState, index: usize) -> Option<i32> {
    if let Some(slot) = state.slots.get(&index) {
        return Some(*slot);
    }
    let slot = state.free_slots.pop()?;
    state.slots.insert(index, slot);
    Some(slot)
}

/// Give back the slots of evicted indices, so a later thumbnail can use them.
fn release_slots(state: &mut GridState, evicted: &[usize]) {
    for index in evicted {
        if let Some(slot) = state.slots.remove(index) {
            state.free_slots.push(slot);
        }
        state.scheduler.uncache(*index);
    }
}

fn release_all_slots(state: &mut GridState) {
    let indices: Vec<usize> = state.slots.keys().copied().collect();
    release_slots(state, &indices);
}

/// Write a composed canvas into an image list slot. `false` when the bitmap couldn't be made,
/// which leaves the cell showing its placeholder rather than a half-written slot.
fn write_slot(images: HIMAGELIST, slot: i32, side: u32, canvas: &[u8]) -> bool {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side as i32,
            // Negative, so the rows are top-down and match what `compose_slot` writes.
            biHeight: -(side as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: the header describes the buffer the call allocates, and `bits` receives it.
    let bitmap = unsafe {
        CreateDIBSection(
            Some(HDC::default()),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
    };
    let Ok(bitmap) = bitmap else { return false };
    if bits.is_null() {
        // SAFETY: a bitmap we just created and nothing holds.
        let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
        return false;
    }
    // SAFETY: the section is exactly `side * side * 4` bytes, which is what `compose_slot`
    // returned, and the two don't overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(canvas.as_ptr(), bits.cast::<u8>(), canvas.len());
    }
    // SAFETY: a live image list of ours, a slot inside its declared count, and a bitmap of the
    // list's own size. `None` for the mask, because the slot is opaque.
    let replaced = unsafe { ImageList_Replace(images, slot, bitmap, None) };
    // The image list copies the bits, so the bitmap is ours to free either way.
    // SAFETY: nothing holds it now.
    let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
    replaced.as_bool()
}

/// Rebuild the image list at a new DPI. Every thumbnail is regenerated, because an image list's
/// slot size is fixed when it's created.
pub(super) fn rescale(dpi: u32) {
    let Some(Some((grid, images, old, count))) = with_ui_mut(|ui| {
        let side = layout::scale(layout::CELL_THUMBNAIL, dpi).max(1) as u32;
        if side == ui.grid_state.slot_side {
            return None;
        }
        let images = create_image_list(side)?;
        let old = ui.grid_state.images;
        ui.grid_state.images = images;
        ui.grid_state.slot_side = side;
        ui.grid_state.slots.clear();
        ui.grid_state.free_slots = (0..SLOTS as i32).rev().collect();
        ui.grid_state.cache = ThumbnailCache::with_budget(SLOTS * thumbnail::slot_bytes(side));
        let count = ui.grid_state.model.len();
        ui.grid_state
            .scheduler
            .set_folder(count, grid_model::clamp_visible_range(0..0, count));
        Some((ui.grid, images, old, count))
    }) else {
        return;
    };

    // SAFETY: a live listview of ours and an image list this module owns.
    unsafe {
        SendMessageW(
            grid,
            LVM_SETIMAGELIST,
            Some(WPARAM(LVSIL_NORMAL as usize)),
            Some(LPARAM(images.0)),
        )
    };
    // SAFETY: the list the control has just stopped using.
    let _ = unsafe { ImageList_Destroy(Some(old)) };
    apply_icon_spacing(grid, dpi);
    set_item_count(grid, count);
    pump_visible_range();
}

// ── Notifications ────────────────────────────────────────────────────────────

/// A `WM_NOTIFY` the listview sent.
pub(super) fn notify(header: *mut NMHDR) -> LRESULT {
    // SAFETY: `WM_NOTIFY` hands us a live `NMHDR` that outlives the call, and every struct below
    // starts with one — which is the whole convention the message is built on. The pointer stays
    // a pointer rather than becoming a `&NMHDR` first, because `LVN_GETDISPINFO` needs to WRITE
    // through it and casting a shared reference to a mutable one is undefined behaviour.
    let code = unsafe { (*header).code };
    match code {
        LVN_GETDISPINFOW => {
            // SAFETY: for this notification the header is the first field of an `NMLVDISPINFOW`,
            // and the item inside it is ours to fill.
            let info = unsafe { &mut *header.cast::<NMLVDISPINFOW>() };
            fill_item(&mut info.item);
            LRESULT(0)
        }
        LVN_ODCACHEHINT => {
            // SAFETY: same shape, an `NMLVCACHEHINT`.
            let hint = unsafe { &*header.cast::<NMLVCACHEHINT>() };
            cache_hint(hint.iFrom as usize, hint.iTo as usize);
            LRESULT(0)
        }
        LVN_ITEMCHANGED => {
            // SAFETY: same shape, an `NMLISTVIEW`.
            let changed = unsafe { &*header.cast::<NMLISTVIEW>() };
            if changed.uChanged.0 == LVIF_STATE.0
                && changed.uNewState & LVIS_SELECTED.0 != 0
                && changed.uOldState & LVIS_SELECTED.0 == 0
                && changed.iItem >= 0
            {
                crate::commands::send_command(crate::commands::AppCommand::BrowseGridSelected(
                    changed.iItem as usize,
                ));
            }
            LRESULT(0)
        }
        NM_DBLCLK => {
            crate::commands::send_command(crate::commands::AppCommand::BrowseOpenSelected);
            LRESULT(0)
        }
        _ => LRESULT(0),
    }
}

/// Answer the listview's question about one cell: its filename, and which image list slot to
/// draw. `-1` is "no image", which leaves the cell blank until its thumbnail lands.
fn fill_item(item: &mut LVITEMW) {
    let index = item.iItem as usize;
    with_ui_mut(|ui| {
        if item.mask.0 & LVIF_TEXT.0 != 0 {
            let name = ui
                .grid_state
                .model
                .path(index)
                .and_then(|path| PathPolicy::windows().file_name(path))
                .unwrap_or("");
            // The listview reads the pointer after this returns, so the buffer has to outlive
            // the notification. One cell is asked about at a time, so one buffer is enough.
            ui.grid_state.label = wide(name);
            item.pszText = windows::core::PWSTR(ui.grid_state.label.as_mut_ptr());
        }
        if item.mask.0 & LVIF_IMAGE.0 != 0 {
            item.iImage = ui.grid_state.slots.get(&index).copied().unwrap_or(-1);
        }
    });
}

/// The listview says it is about to want this range, which is the cheapest scroll signal it
/// gives. Widen the scheduler onto it and keep generating.
fn cache_hint(from: usize, to: usize) {
    with_ui_mut(|ui| {
        let len = ui.grid_state.model.len();
        let range = grid_model::clamp_visible_range(from..to.saturating_add(1), len);
        ui.grid_state.scheduler.set_visible_range(range.clone());
        ui.grid_state.cache.set_visible_range(range);
        let evicted = ui.grid_state.cache.evict_to_budget();
        release_slots(&mut ui.grid_state, &evicted);
        pump(ui);
    });
}
