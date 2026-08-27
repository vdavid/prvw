//! The folder tree: a `SysTreeView32` themed as Explorer's navigation pane.
//!
//! It reads its rows from [`super::super::roots`] and its child folders from
//! [`crate::browser::tree_model`], and it never touches the disk on the main thread — a stale
//! mapped network drive blocks for tens of seconds, and that would freeze winit's pump along with
//! everything else. A node's children are a [`ChildCache`] state machine filled by a
//! [`TreeScanner`] thread, exactly as the macOS outline view fills its own.
//!
//! ## Decision: `SetWindowTheme(tree, "Explorer")`
//!
//! **Why:** it is the difference between Explorer's chevrons with hot-tracking and a Windows 95
//! treeview with plus-and-minus boxes. Nothing else in this milestone changes the impression as
//! much per line. `dark_mode::apply_to_window` makes the call, with `DarkMode_Explorer` when the
//! system is dark, so the light and dark paths are one call site rather than two.
//!
//! ## Decision: one generic folder icon for every row
//!
//! **Why:** real per-row shell icons mean `SHGetFileInfoW` without `SHGFI_USEFILEATTRIBUTES`,
//! and Microsoft's own documentation says icon extraction "generally should not be called from a
//! UI thread". For a mapped drive that is disconnected it can block for tens of seconds, which
//! is precisely the hang the whole tree is built to avoid. So we ask once, with
//! `SHGFI_USEFILEATTRIBUTES` and `FILE_ATTRIBUTE_DIRECTORY`, which answers from the registry
//! without touching a disk, and every row wears that icon. The visible cost is that a drive row
//! shows a folder rather than a disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::{
    HTREEITEM, NMHDR, NMTREEVIEWW, TVE_EXPAND, TVGN_CARET, TVGN_PARENT, TVI_LAST, TVI_ROOT,
    TVIF_CHILDREN, TVIF_IMAGE, TVIF_PARAM, TVIF_SELECTEDIMAGE, TVIF_TEXT, TVINSERTSTRUCTW,
    TVINSERTSTRUCTW_0, TVITEMEXW, TVITEMEXW_CHILDREN, TVITEMW, TVM_DELETEITEM, TVM_EXPAND,
    TVM_GETITEMW, TVM_GETNEXTITEM, TVM_INSERTITEMW, TVM_SELECTITEM, TVM_SETIMAGELIST, TVM_SETITEMW,
    TVN_ITEMEXPANDINGW, TVN_SELCHANGEDW, TVS_HASBUTTONS, TVS_HASLINES, TVS_LINESATROOT,
    TVS_SHOWSELALWAYS, TVS_TRACKSELECT, WC_TREEVIEWW,
};
use windows::Win32::UI::Shell::{
    SHFILEINFOW, SHGFI_SMALLICON, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, HMENU, SendMessageW, WS_BORDER, WS_CHILD, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::PCWSTR;

use crate::browser::tree_model::{self, ChildCache, Root, TreeScanner};
use crate::paths::PathPolicy;

use super::{ID_TREE, Ui, set_font, wide, with_ui, with_ui_mut};

/// `TVSIL_NORMAL`, from `commctrl.h`. The image list a treeview draws beside every row.
const TVSIL_NORMAL: u32 = 0;
/// `TVS_EX_DOUBLEBUFFER`, and the message that sets the extended styles.
const TVM_SETEXTENDEDSTYLE: u32 = 0x1100 + 44;
const TVS_EX_DOUBLEBUFFER: u32 = 0x0004;
/// `FILE_ATTRIBUTE_DIRECTORY`, from `winnt.h`.
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;

/// Everything the tree knows that isn't a window handle.
pub(super) struct TreeState {
    /// The top-level rows: known folders, then drives.
    roots: Vec<Root>,
    /// Per-path child folders, and their load state. The tree serves children from here and
    /// never reads a directory itself.
    children: ChildCache,
    /// The background directory scanner that fills the cache.
    scanner: TreeScanner,
    /// Every path the tree has a row for. A row carries its index here in its `lParam`, because
    /// `HTREEITEM` is an opaque handle with nowhere to hang a `PathBuf`.
    paths: Vec<PathBuf>,
    /// The row for a path, keyed case-insensitively because NTFS is and the same folder can
    /// arrive spelled two ways (argv, `canonicalize`, a drive enumeration).
    rows: HashMap<String, HTREEITEM>,
    /// The reveal walk in flight, if any: the root-to-target chain and how far along it we are.
    reveal: Option<Reveal>,
    /// The row image every node wears. See the module's icon decision.
    icon: i32,
}

/// A browse-open reveal in progress. The chain comes from `tree_model::reveal_path_chain`, and
/// each step waits for that folder's children to arrive before expanding the next.
struct Reveal {
    chain: Vec<PathBuf>,
    position: usize,
}

/// Build the treeview and its state. The rows go on immediately; their children don't, because
/// reading them is the scanner's job.
pub(super) fn create(
    parent: HWND,
    instance: HINSTANCE,
    font: HFONT,
    theme: crate::platform::windows::dark_mode::Theme,
    _dpi: u32,
) -> Option<(HWND, TreeState)> {
    // `TVS_TRACKSELECT` is Explorer's hot-tracking; `TVS_SHOWSELALWAYS` keeps the selected
    // folder visible while the grid has focus, which is what makes Tab legible.
    let style = WS_CHILD
        | WS_VISIBLE
        | WS_TABSTOP
        | WS_BORDER
        | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
            TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_SHOWSELALWAYS | TVS_TRACKSELECT,
        );
    // SAFETY: `WC_TREEVIEWW` is registered by `InitCommonControlsEx` with `ICC_TREEVIEW_CLASSES`,
    // and `parent` is a live window of ours.
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            WC_TREEVIEWW,
            PCWSTR::null(),
            style,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(ID_TREE as isize as *mut std::ffi::c_void)),
            Some(instance),
            None,
        )
    }
    .ok()?;

    set_font(hwnd, font);
    // The single highest-value line in this milestone: Explorer's chevrons rather than Windows
    // 95's plus boxes, and the dark variant when the system is dark.
    crate::platform::windows::dark_mode::apply_to_window(hwnd, theme);
    // SAFETY: a live treeview of ours. Double buffering is what stops the rows flickering while
    // a scan fills them in.
    unsafe {
        SendMessageW(
            hwnd,
            TVM_SETEXTENDEDSTYLE,
            Some(WPARAM(TVS_EX_DOUBLEBUFFER as usize)),
            Some(LPARAM(TVS_EX_DOUBLEBUFFER as isize)),
        )
    };

    let icon = attach_system_image_list(hwnd);

    let mut state = TreeState {
        roots: super::super::shell_roots::enumerate(),
        children: ChildCache::new(),
        scanner: TreeScanner::start(),
        paths: Vec::new(),
        rows: HashMap::new(),
        reveal: None,
        icon,
    };
    populate_roots(hwnd, &mut state);
    Some((hwnd, state))
}

/// Hand the treeview the system image list and answer with the generic folder icon's index.
/// `SHGFI_USEFILEATTRIBUTES` is what keeps this off the disk; see the module's icon decision.
fn attach_system_image_list(tree: HWND) -> i32 {
    let mut info = SHFILEINFOW::default();
    let name = wide("folder");
    // SAFETY: the path is never touched because `SHGFI_USEFILEATTRIBUTES` says to answer from
    // the attributes instead, and the struct is ours to fill.
    let list = unsafe {
        SHGetFileInfoW(
            PCWSTR(name.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(
                FILE_ATTRIBUTE_DIRECTORY,
            ),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
        )
    };
    if list == 0 {
        return 0;
    }
    // The system image list belongs to the shell and must never be destroyed, which is why the
    // handle is set and then forgotten rather than stored.
    // SAFETY: a live treeview of ours, and a shell-owned image list that outlives the process.
    unsafe {
        SendMessageW(
            tree,
            TVM_SETIMAGELIST,
            Some(WPARAM(TVSIL_NORMAL as usize)),
            Some(LPARAM(list as isize)),
        )
    };
    info.iIcon
}

/// Put the top-level rows on. Each claims one child so a chevron shows before anything has been
/// scanned; the first expand replaces the claim with the truth.
fn populate_roots(tree: HWND, state: &mut TreeState) {
    let roots = std::mem::take(&mut state.roots);
    for root in &roots {
        insert_row(tree, state, TVI_ROOT, &root.name, &root.path);
    }
    log::info!("Browse tree: {} root(s)", roots.len());
    state.roots = roots;
}

/// Add one row under `parent`, remembering its path.
fn insert_row(
    tree: HWND,
    state: &mut TreeState,
    parent: HTREEITEM,
    label: &str,
    path: &Path,
) -> Option<HTREEITEM> {
    let index = state.paths.len();
    state.paths.push(path.to_path_buf());
    let text = wide(label);
    let insert = TVINSERTSTRUCTW {
        hParent: parent,
        hInsertAfter: TVI_LAST,
        Anonymous: TVINSERTSTRUCTW_0 {
            itemex: TVITEMEXW {
                mask: TVIF_TEXT | TVIF_PARAM | TVIF_CHILDREN | TVIF_IMAGE | TVIF_SELECTEDIMAGE,
                pszText: windows::core::PWSTR(text.as_ptr().cast_mut()),
                lParam: LPARAM(index as isize),
                // Assume expandable until a scan proves otherwise: finding out costs a directory
                // read, and this is the main thread.
                cChildren: TVITEMEXW_CHILDREN(1),
                iImage: state.icon,
                iSelectedImage: state.icon,
                ..Default::default()
            },
        },
    };
    // SAFETY: a live treeview of ours; the insert struct and the text outlive the call.
    let item = unsafe {
        SendMessageW(
            tree,
            TVM_INSERTITEMW,
            None,
            Some(LPARAM(std::ptr::from_ref(&insert) as isize)),
        )
    };
    if item.0 == 0 {
        state.paths.pop();
        return None;
    }
    let item = HTREEITEM(item.0);
    state.rows.insert(row_key(path), item);
    Some(item)
}

/// The map key for a path. Case-folded, because NTFS is case-insensitive and one folder reaches
/// the tree spelled three ways: what the user typed, what `canonicalize` returned, and what a
/// drive enumeration produced.
fn row_key(path: &Path) -> String {
    crate::paths::PathPolicy::windows()
        .display(path)
        .to_lowercase()
}

/// The path a row stands for.
fn row_path(tree: HWND, state: &TreeState, item: HTREEITEM) -> Option<PathBuf> {
    let mut query = TVITEMW {
        mask: TVIF_PARAM,
        hItem: item,
        ..Default::default()
    };
    // SAFETY: a live treeview of ours, and the item struct is ours to fill.
    let read = unsafe {
        SendMessageW(
            tree,
            TVM_GETITEMW,
            None,
            Some(LPARAM(std::ptr::from_mut(&mut query) as isize)),
        )
    };
    if read.0 == 0 {
        return None;
    }
    state.paths.get(query.lParam.0 as usize).cloned()
}

// ── Notifications ────────────────────────────────────────────────────────────

/// A `WM_NOTIFY` the treeview sent. Everything the tree does that isn't native starts here.
pub(super) fn notify(header: *mut NMHDR) -> LRESULT {
    // SAFETY: `WM_NOTIFY` hands us a live `NMHDR` that outlives the call, and both structs below
    // start with one.
    let code = unsafe { (*header).code };
    match code {
        TVN_ITEMEXPANDINGW => {
            // SAFETY: for the tree's expand notifications, the header is the first field of an
            // `NMTREEVIEWW`.
            let notification = unsafe { &*header.cast::<NMTREEVIEWW>() };
            if notification.action == TVE_EXPAND {
                request_children(notification.itemNew.hItem);
            }
            LRESULT(0)
        }
        TVN_SELCHANGEDW => {
            // SAFETY: same shape as above.
            let notification = unsafe { &*header.cast::<NMTREEVIEWW>() };
            selection_changed(notification.itemNew.hItem);
            LRESULT(0)
        }
        _ => LRESULT(0),
    }
}

/// Start a scan for a node's children, unless one has already run or is running. The node shows
/// nothing until [`children_loaded`] arrives, which is the point: the alternative is a directory
/// read on the main thread.
fn request_children(item: HTREEITEM) {
    let Some(Some(path)) = with_ui_mut(|ui| {
        let path = row_path(ui.tree, &ui.tree_state, item)?;
        let now = std::time::Instant::now();
        ui.tree_state
            .children
            .begin_scan(&path, now)
            .then_some(path)
    }) else {
        return;
    };
    with_ui(|ui| ui.tree_state.scanner.scan(path));
    super::refresh_status_bar();
}

/// A row was selected: tell the app, so the grid lists that folder.
fn selection_changed(item: HTREEITEM) {
    let Some(Some(path)) = with_ui(|ui| row_path(ui.tree, &ui.tree_state, item)) else {
        return;
    };
    crate::commands::send_command(crate::commands::AppCommand::BrowseSelectFolder(path));
}

/// Store a finished scan and put its children on the row. Also advances a reveal walk that was
/// waiting on this level.
pub(super) fn children_loaded(path: &Path, children: Vec<PathBuf>) {
    with_ui_mut(|ui| {
        ui.tree_state.children.complete_scan(path, children.clone());
        let Some(&parent) = ui.tree_state.rows.get(&row_key(path)) else {
            return;
        };
        let tree = ui.tree;
        // A second delivery for the same folder (a live re-scan) replaces the rows rather than
        // doubling them.
        remove_children(tree, parent);
        for child in &children {
            let label = PathPolicy::windows()
                .file_name(child)
                .unwrap_or("")
                .to_string();
            insert_row(tree, &mut ui.tree_state, parent, &label, child);
        }
        // Now that the truth is known, a leaf folder loses its chevron.
        set_child_count(tree, parent, i32::from(!children.is_empty()));
    });
    advance_reveal();
    super::refresh_status_bar();
}

/// Take every row under `parent` off, so a re-scan replaces rather than duplicates.
fn remove_children(tree: HWND, parent: HTREEITEM) {
    // `TVGN_CHILD` is 4, from `commctrl.h`.
    const TVGN_CHILD: u32 = 0x0004;
    loop {
        // SAFETY: a live treeview of ours.
        let child = unsafe {
            SendMessageW(
                tree,
                TVM_GETNEXTITEM,
                Some(WPARAM(TVGN_CHILD as usize)),
                Some(LPARAM(parent.0)),
            )
        };
        if child.0 == 0 {
            return;
        }
        // SAFETY: a row of ours, which the treeview owns until this call.
        unsafe { SendMessageW(tree, TVM_DELETEITEM, None, Some(LPARAM(child.0))) };
    }
}

fn set_child_count(tree: HWND, item: HTREEITEM, count: i32) {
    let mut update = TVITEMW {
        mask: TVIF_CHILDREN,
        hItem: item,
        cChildren: TVITEMEXW_CHILDREN(count),
        ..Default::default()
    };
    // SAFETY: a live treeview of ours; the struct outlives the call.
    unsafe {
        SendMessageW(
            tree,
            TVM_SETITEMW,
            None,
            Some(LPARAM(std::ptr::from_mut(&mut update) as isize)),
        )
    };
}

// ── The reveal walk ──────────────────────────────────────────────────────────

/// Start expanding from the containing root down to `folder`, then select it. Browse-open
/// positioning: entering browse from an image opens already showing where you are.
///
/// The walk can't run synchronously, because expanding a node it hasn't scanned yet would find
/// no children. So it expands one level, waits for [`children_loaded`], and steps on.
pub(super) fn reveal(folder: &Path) {
    // The host wrapper, which on this platform *is* the Windows policy. A Mac asserts the same
    // walk through `reveal_path_chain_under`.
    let Some(Some(chain)) =
        with_ui(|ui| tree_model::reveal_path_chain(&ui.tree_state.roots, folder))
    else {
        log::debug!(
            "Browse tree: nothing to reveal — {} is on no root",
            folder.display()
        );
        return;
    };
    with_ui_mut(|ui| ui.tree_state.reveal = Some(Reveal { chain, position: 0 }));
    advance_reveal();
}

/// True while a reveal walk is still in flight, so QA can wait for it to settle.
pub(super) fn reveal_pending(ui: &Ui) -> bool {
    ui.tree_state.reveal.is_some()
}

/// Take the walk as far as the rows that exist allow: expand each ancestor whose children are
/// already loaded, and stop at the first that isn't (its scan is in flight, and the delivery
/// calls back here). At the target, select it — which fires `BrowseSelectFolder` and lists the
/// folder for the grid.
fn advance_reveal() {
    loop {
        let step = with_ui_mut(|ui| {
            let reveal = ui.tree_state.reveal.as_ref()?;
            let path = reveal.chain.get(reveal.position)?.clone();
            let last = reveal.position + 1 == reveal.chain.len();
            let item = *ui.tree_state.rows.get(&row_key(&path))?;
            Some((path, item, last))
        });
        let Some(Some((path, item, last))) = step else {
            // The row isn't there yet (its parent's scan hasn't landed) or there is no walk.
            return;
        };
        if last {
            select(item);
            with_ui_mut(|ui| ui.tree_state.reveal = None);
            log::debug!("Browse tree revealed {}", path.display());
            return;
        }
        let loaded = with_ui(|ui| ui.tree_state.children.loaded(&path).is_some()) == Some(true);
        expand(item);
        if !loaded {
            // `expand` asked for the scan through `TVN_ITEMEXPANDING`; the delivery resumes us.
            return;
        }
        with_ui_mut(|ui| {
            if let Some(reveal) = ui.tree_state.reveal.as_mut() {
                reveal.position += 1;
            }
        });
    }
}

fn expand(item: HTREEITEM) {
    let Some(tree) = with_ui(|ui| ui.tree) else {
        return;
    };
    // SAFETY: a live treeview of ours and a row of its own.
    unsafe {
        SendMessageW(
            tree,
            TVM_EXPAND,
            Some(WPARAM(TVE_EXPAND.0 as usize)),
            Some(LPARAM(item.0)),
        )
    };
}

fn select(item: HTREEITEM) {
    let Some(tree) = with_ui(|ui| ui.tree) else {
        return;
    };
    // SAFETY: a live treeview of ours and a row of its own. This fires `TVN_SELCHANGED`, which
    // is what lists the folder.
    unsafe {
        SendMessageW(
            tree,
            TVM_SELECTITEM,
            Some(WPARAM(TVGN_CARET as usize)),
            Some(LPARAM(item.0)),
        )
    };
}

/// Explorer's convention: Backspace goes to the parent folder. It costs nothing here because in
/// browse mode Backspace has no other job — in image mode it means Previous — and it is handled
/// as a native move rather than routed as a command, exactly as the arrow keys are.
pub(super) fn select_parent() {
    let Some(tree) = with_ui(|ui| ui.tree) else {
        return;
    };
    // SAFETY: a live treeview of ours.
    let current = unsafe {
        SendMessageW(
            tree,
            TVM_GETNEXTITEM,
            Some(WPARAM(TVGN_CARET as usize)),
            Some(LPARAM(0)),
        )
    };
    if current.0 == 0 {
        return;
    }
    // SAFETY: same treeview, and a row it just handed us.
    let parent = unsafe {
        SendMessageW(
            tree,
            TVM_GETNEXTITEM,
            Some(WPARAM(TVGN_PARENT as usize)),
            Some(LPARAM(current.0)),
        )
    };
    if parent.0 != 0 {
        select(HTREEITEM(parent.0));
    }
}

/// True while a directory scan the user is waiting on hasn't come back, so the status bar can
/// say "Loading…" rather than a count that would climb while they read it.
pub(super) fn scan_pending(ui: &Ui) -> bool {
    tree_model::scan_overdue(
        ui.tree_state.children.earliest_in_flight(),
        std::time::Instant::now(),
    )
}

/// The root paths, for the live-folder-sync watch.
pub(super) fn root_paths(ui: &Ui) -> Vec<PathBuf> {
    ui.tree_state.roots.iter().map(|r| r.path.clone()).collect()
}

/// Forget a folder's cached children so the next expand re-scans it. Live folder sync calls this
/// when a watched folder changes on disk.
pub(super) fn invalidate(path: &Path) {
    with_ui_mut(|ui| ui.tree_state.children.invalidate(path));
    with_ui(|ui| ui.tree_state.scanner.scan(path.to_path_buf()));
}
