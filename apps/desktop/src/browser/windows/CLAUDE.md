# Browse mode on Windows

Explorer's shape and ACDSee's: a `SysTreeView32` folder tree, a virtual `SysListView32` thumbnail grid, a splitter the
container draws itself, and a status bar along the bottom. The macOS twin is `browser/` (an `NSOutlineView` plus an
`NSCollectionView`); this is deliberately **not** a port of it. Full design:
[`docs/specs/windows-ui-design.md`](../../../../docs/specs/windows-ui-design.md) → "Browse mode".

| File                | Purpose                                                                                                      |
| ------------------- | ------------------------------------------------------------------------------------------------------------ |
| `roots.rs`          | What the tree shows at its top level: known folders, drive letters, their labels, hidden-entry policy. Pure  |
| `layout.rs`         | Where the four children go, in device pixels at the monitor's DPI, plus the grid's visible-range maths. Pure |
| `status.rs`         | What the status bar's three panes say. Pure                                                                  |
| `keys.rs`           | The three keys the panes take, and the many they leave to the controls. Pure                                 |
| `thumbnail.rs`      | Preview pixels → the square, top-down BGRA canvas an image list slot takes. Pure                             |
| `selection_meta.rs` | One worker measuring the selected image's pixel size, for the status bar                                     |
| `shell_roots.rs`    | The Win32 half of `roots`: `SHGetKnownFolderPath`, `GetLogicalDrives`, `GetVolumeInformationW`               |
| `ui/mod.rs`         | The container window, its procedure, the layout, the status bar, the splitter, the panes' key subclass       |
| `ui/tree.rs`        | The treeview: rows, lazy children, selection, the reveal walk                                                |
| `ui/grid.rs`        | The listview: virtual items, the image list, thumbnails, selection                                           |

**The split.** Everything but `shell_roots` and `ui` compiles on every platform and is tested on a Mac. The Win32 layer
decides as little as it can, which is the split that made the settings dialog land in one pass. Nothing in `ui/` has
ever executed on Windows.

## The swap, and why it's safe

Image mode is a wgpu surface on winit's HWND; browse mode is a **single container child window** covering the whole
client area. They never overlap: one is shown and the other hidden.

macOS needs this because a transparent Metal pixel occludes what's behind it. Windows needs it for a different reason:
wgpu's DX12 backend uses a **flip-model swapchain**, which composes as its own DWM visual rather than respecting GDI's
child-window clipping, so a control "in front of" it is not reliably in front of it. One window to show and hide is safe
by construction either way.

`WS_CLIPCHILDREN` goes on **winit's own window** when the container is built — winit doesn't set it and has no API to
ask for it — and showing the container puts it at `HWND_TOP`.

**Gotcha: hiding a focused window doesn't move the focus.** **Why:** the pane keeps it, and image mode is then dead to
the keyboard. `set_hidden(true)` calls `SetFocus` on winit's window, which is the Win32 form of what
`restore_content_view_first_responder` does on macOS.

**Decision: stop presenting while the browser is up.** **Why:** `App::enter_view_mode` clears the image, paints one
frame, and then stops asking for redraws, so no `Present` happens while browse mode is visible and there is nothing to
paint over the controls. It also lets the GPU go idle, which is what `AGENTS.md`'s "respect resources" asks for. The
container repaints itself opaquely in `WM_ERASEBKGND` and owns the client area.

## Where the state lives

**Decision: one thread-local `RefCell<Option<Ui>>`, not a struct the caller holds.** **Why:** a window procedure has to
reach the models — `LVN_GETDISPINFO` asks for a cell's text while `WM_NOTIFY` is on the stack — and it has no way to
borrow through whatever `browser::State` is holding. `settings::windows::dialog` does the same, for the same reason.
`BrowseUi` is therefore an empty handle; everything real is in `UI`.

**Gotcha: never hold the borrow across a call into Win32.** **Why:** a Win32 call that dispatches synchronously reaches
a window procedure, and that procedure reaches back in here. A `RefCell` already borrowed panics, and a panic inside a
window procedure unwinds across an `extern "system"` boundary, which Rust turns into a **process abort**. So the failure
isn't a wrong pixel: the app is gone, with nothing to report.

This is not hypothetical. `BrowseUi::apply_focus` held the borrow across `SetFocus`, which sends `WM_KILLFOCUS` and
`WM_SETFOCUS` synchronously; the focus change repainted, `container_proc` borrowed again, and the process aborted. That
fired on **every** entry into browse mode, because `sync_native` applies the focus each time. The clue in a log is
`RefCell already mutably borrowed` followed by `panic in a function that cannot unwind`.

`SendMessageW` is the obvious offender, and it is not the only one. `SetFocus`, `SetWindowPos`, `ShowWindow`,
`MoveWindow`, `DestroyWindow`, `EnableWindow`, `SetParent`, and `UpdateWindow` all dispatch. So does anything that wraps
one: `set_font` is a `WM_SETFONT`, and a helper on `Ui` looks like state but may be a message. **The call being indirect
is what hid both instances**, so check what a closure calls, not only what it spells.

The shape that works: `with_ui` / `with_ui_mut` take the borrow, read out the handles, and drop it; the Win32 call comes
after. Where the work needs two rounds, split it (`Ui::begin_rescale` decides and hands back the handles,
`Ui::finish_rescale` records what the controls answered).

**The net:** both helpers use `try_borrow` rather than `borrow`. A re-entrant access declines, logs an error naming the
call site, and the app carries on with one skipped repaint. That turns this class of mistake from a silent process abort
into a line in the log the E2E harness quotes back. It is a net, not a licence — an error line from there means a call
site is breaking the rule above.

## The tree

- **`SetWindowTheme(tree, "Explorer")` is the single highest-value line here.** Without it the treeview draws Windows 95
  plus-and-minus boxes; with it, Explorer's chevrons and hot-tracking. It comes through
  `platform::windows::dark_mode::apply_to_window`, which picks `DarkMode_Explorer` when the system is dark, so light and
  dark are one call site.
- **A row's path lives in an arena, and its index in the row's `lParam`.** `HTREEITEM` is an opaque handle with nowhere
  to hang a `PathBuf`, and boxing one per row would leak on `TVM_DELETEITEM`.
- **The top-level rows lead with Pictures, Desktop, and Downloads, then Home, then the drives.** Home is the user's
  profile, read from `%USERPROFILE%` (falling back to `FOLDERID_Profile`), the way macOS reads `$HOME`. It earns its
  place on the reveal walk: a chain reveals under the longest-matching root, so a folder in the profile is two or three
  levels from Home rather than six from `C:\` — and the walk lists every level it passes, one of which would otherwise
  be the machine's temp directory, which on a busy machine holds thousands of entries.
- **Rows are looked up through `PathPolicy::windows().key()`**, never on the `PathBuf`. NTFS is case-insensitive and one
  folder reaches the tree spelled three ways: what the user typed, what `canonicalize` returned, and what a drive
  enumeration produced.
- **Children never load on the main thread.** A node claims one child so a chevron shows, the first expand starts a
  shared `folder_scan` scan, and `FolderScanned` fills the rows and corrects the claim. Same `ChildCache` state machine
  macOS uses.
- **The reveal walk is asynchronous for the same reason**: expanding an unscanned node would find no children. It
  expands one level, waits for the delivery, and steps on.

**Gotcha: a reveal walk stalls on a hidden ancestor unless it asks for one.** **Why:** the tree hides what Explorer
hides, and every Windows temp folder lives under `AppData`, which carries the hidden attribute. A folder dropped from
there would get no row to expand and the walk would wait forever. `request_children` therefore names the walk's next
step (`folder_scan::FolderScanner::request_revealing`), and that one child is listed however hidden it is. Nothing else
in the same directory is.

**Gotcha: a `HashMap<PathBuf, _>` is the same mistake `==` is, and it stalls the walk.** **Why:** the reveal chain is
built from the canonicalized target, so it names `\\?\C:\Users`, while the scan that fills that node was asked for under
the row's own spelling, `C:\Users`. A byte-keyed cache misses, `advance_reveal` decides the children haven't landed, and
it waits for a delivery that already happened — forever, with the tree sitting on the drive root and
`browse_reveal_pending` stuck true. `ChildCache` keys through `PathPolicy::key` for exactly this. The clue in a log is
`Browse: selected folder C:\` and nothing after it.

**Decision: every decision the walk makes lives in `tree_model::RevealWalk`, and this module only carries them out.**
**Why:** the walk is driven by events from three sources — a scan it asked for, a scan somebody else asked for, and a
live re-scan that deletes the rows underneath it — which is exactly the shape where "it obviously terminates" stops
being true. Kept here it was a `loop` in a window procedure that nothing on a Mac could execute, let alone test. Moved
there it is a state machine with the termination argument written on it (`RevealWalk` → "why it terminates"), a hard
step budget as a backstop, and a driver in its tests that runs the whole walk against a fake tree from any host,
including one that keeps wiping its rows. `advance_reveal` is now a `match` with no decisions of its own.

**Gotcha: a treeview picks its own first row the moment it takes focus.** **Why:** `SetFocus` on a treeview with no
selection selects the first visible row, and deleting the selected row makes it pick a neighbour. Both arrive as
`TVN_SELCHANGED` with `action == TVC_UNKNOWN`, and neither is a folder anyone chose to look at. Acting on them listed
the drive root at every browse entry and spent the one-shot browse-open state (the grid preselect, the focus move) on
that listing before the reveal had landed. A click is `TVC_BYMOUSE` and an arrow key is `TVC_BYKEYBOARD`, so the only
`TVC_UNKNOWN` worth honouring is one `select` asked for itself, which is what its `selecting` marker says.

**Decision: one generic folder icon for every row.** **Why:** per-row shell icons mean `SHGetFileInfoW` without
`SHGFI_USEFILEATTRIBUTES`, which Microsoft's own docs say should not be called from a UI thread, and which blocks for
tens of seconds on a disconnected mapped drive. We ask once, with the attributes flag, and every row wears that answer.
The visible cost is that a drive row shows a folder rather than a disk.

**Gotcha: `GetVolumeInformationW` blocks on a disconnected network drive.** **Why:** it waits out the SMB timeout, on
the event loop's thread. `shell_roots` never asks a `DRIVE_REMOTE` letter for its label; it gets its drive-type name
instead.

**Gotcha: touching an empty optical drive puts up a system dialog.** **Why:** Windows shows "There is no disk in the
drive", which runs its own message loop. `SetThreadErrorMode(SEM_FAILCRITICALERRORS)` around the enumeration is what
Explorer sets to suppress it.

## The grid

- **`LVS_OWNERDATA` makes it virtual.** A 5,000-image folder costs one `LVM_SETITEMCOUNT`; the listview asks for a
  cell's text and image as it draws.
- **Thumbnails come from `previews::generator`**, which on Windows reads Explorer's own `thumbcache_*.db` through
  `IShellItemImageFactory`. A folder Explorer has visited paints from cache rather than from a decode.

**Decision: a fixed pool of image list slots, recycled by hand.** **Why:** an `HIMAGELIST` can't remove an image without
renumbering every image after it, and eviction is the whole point of the byte-budget cache. The list is created at a
fixed count, a slot is claimed on arrival and returned on eviction, and `ImageList_Replace` writes into a slot that
keeps its number. The cache's budget is computed from the same count, so eviction can never fail for want of a slot.

**Decision: the visible range is arithmetic, not a message.** **Why:** `LVM_GETTOPINDEX` and `LVM_GETCOUNTPERPAGE` are
documented for report and list view only, and in icon view the second answers with the whole folder.
`layout::visible_range` computes it from `LVM_GETORIGIN` and the icon spacing we set ourselves — and being arithmetic,
it is asserted from a Mac. `LVN_ODCACHEHINT` widens it on scroll.

**Gotcha: a `LVN_GETDISPINFO` text pointer has to outlive the notification.** **Why:** the listview reads `pszText`
after the handler returns. The buffer lives in `GridState::label`; one cell is asked about at a time, so one buffer is
enough.

**Gotcha: the letterbox colour is the listview's background, not the dialog's.** **Why:** `Theme::colors` is the grey a
settings window paints on, and a thumbnail letterboxed against it would sit in a visible box on a white grid.
`pane_background` asks for `COLOR_WINDOW` in light mode — asked for rather than named, so a high-contrast scheme's own
colour comes back — and comctl32's near-black list background in dark.

**Gotcha: image list bitmaps are BGRA and top-down.** **Why:** a DIB stores blue first, and the DIB section is created
with a negative `biHeight`. `thumbnail::compose_slot` does both conversions in the pass that letterboxes, and its tests
pin the row order — getting it backwards shows every thumbnail upside down.

## Keyboard

The panes hold the focus, so winit delivers no keyboard input in browse mode and `input::browse_key_to_command` never
fires here. A `SetWindowSubclass` on each pane takes Tab, Enter, and Esc (`keys::browse_keydown_command`) and lets
everything else through, which is what keeps arrow selection, page keys, Home, End, and type-select native. Backspace
goes to the parent folder in the tree, as a native move rather than a command.

**Gotcha: a bare-key menu accelerator fires inside the panes.** **Why:** `platform::windows::msg_hook` translates
accelerators against the **main** window whatever has focus, which is right for Ctrl+O and wrong for Home. So `Home` and
`End` are hints on Windows rather than real accelerators, and `input::key_to_command` maps them in image mode, where
winit delivers them. Any future bare-key accelerator has the same problem.

## The status bar

Windows-only by decision (`docs/specs/windows-ui-design.md` → "The browse-mode status bar"). Three panes: how many
images the folder holds, which one is selected, and how big it is. The size comes from `selection_meta`, a worker of its
own — reading a JPEG header is microseconds locally and a round trip on a NAS, and nothing in browse mode touches the
disk on the main thread.

## What still needs a Windows box

All of `ui/` and `shell_roots`. Every call is against documented behaviour and the API shapes are checked by
`./scripts/check.sh --check windows-cross`, but none of it has run. The shared E2E suite (`tests/e2e_shared.rs`, gated
on `BrowseMode` / `BrowseFocus` / `BrowseOpenSelected`) is what will say so first.
