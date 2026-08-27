# Settings on Windows

The Win32 settings dialog: a modeless `SysTabControl32` with six tabs, one Close button, and no OK or Apply. The macOS
window (`../window.rs`) is its counterpart, and the two share the model underneath and nothing above it. Design:
[windows-ui-design.md](../../../../docs/specs/windows-ui-design.md), "The settings surface".

**Nothing in this directory has ever run on Windows.** `dialog.rs` is compile-verified only. That shapes the whole
layout of the module: everything decidable without Win32 is decided in a module that compiles on every platform and is
tested from macOS, and `dialog.rs` creates windows at the rects it's handed without deciding anything.

| File            | Purpose                                                                                                                              | Runs on a Mac |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------ | ------------- |
| `model.rs`      | Every tab, row, description, and trackbar range, plus `apply` (a control's new value → an `AppCommand`) and `value_of` (the reverse) | yes           |
| `layout.rs`     | Where every control goes, in device pixels at the monitor's DPI, plus `ScrollState` for the RAW page                                 | yes           |
| `file_types.rs` | The `HKCU` writes behind "Register Prvw's file types", and the extension list the page shows                                         | yes           |
| `ids.rs`        | `WM_COMMAND` id ↔ (row, part). A collision here is a click changing the wrong setting                                                | yes           |
| `template.rs`   | The in-memory `DLGTEMPLATE` bytes, DWORD-aligned                                                                                     | yes           |
| `theme.rs`      | Dark mode: the two rules are pure, the `uxtheme` ordinals under them are Windows-only                                                | half          |
| `dialog.rs`     | The Win32 layer: create the windows, forward the messages                                                                            | no            |

## Decision: every user-visible string lives in `model.rs`

**Decision:** labels come from `SettingKey::label`, descriptions and group titles from the page tables, and the buttons
from `model::button`. `dialog.rs` writes no copy of its own.

**Why:** it makes `model::user_visible_strings` exhaustive, which is what `the_copy_follows_the_style_guide` and
`titles_are_sentence_case` sweep. `about::content` does the same for the About box; this dialog has thirty times as many
strings, so the sweep matters more here. It also means a Mac can read every word a Windows user will see.

## Decision: the dialog is data, and only the last mile is FFI

**Decision:** `model.rs` holds the six pages as `const` tables. `dialog.rs` walks `Tab::ALL`, asks `model::page` what a
tab holds, and creates one control per row. Adding a setting is a row in a table.

**Why:** it moves the part that can be wrong onto a machine that can test it.
`a_row_writes_its_own_field_and_nothing_else` checks all 39 rows against `SettingKey::field`, so a copy-paste slip that
pointed the clarity radius at `clarity_amount` fails on a Mac rather than on a user's RAW file. The same shape gives
`no_two_controls_overlap` at five scale factors.

## Decision: controls in code, never an `.rc` template

**Decision:** the `DLGTEMPLATE` in `template.rs` carries **zero** controls, and every control is created afterwards with
`CreateWindowExW`.

**Why:** a template's control entries hardcode their label strings. That would sever the link between a settings row and
the `SettingKey` it satisfies, which is what the parity harness exists to create (`../CLAUDE.md`, "Why a row can't skip
the registry"). Zero controls also sidesteps dialog units: a template's `cx` and `cy` are in units derived from the
dialog's font, and the size arrives through `SetWindowPos` in device pixels instead.

## Decision: apply on release, not during the drag

**Decision:** a trackbar's label follows every `WM_HSCROLL`, but the setting is written only on a discrete step or
`TB_ENDTRACK`. `SB_THUMBTRACK` updates the label and nothing else.

**Why:** each RAW change costs a full decode, tens of milliseconds on a 20 MP file. A drag across a 100-step track would
queue a hundred of them. The macOS RAW sliders make the same call with `setContinuous(false)`, for the same reason.

## Decision: File associations registers, and says who owns the choice

**Decision:** the page is a paragraph, a read-only list of extensions, "Register Prvw's file types", and "Open Windows
default apps settings". Not 16 per-format toggles.

**Why:** Windows removed programmatic default-handler setting in 10 20H2. `UserChoice` is hash-protected, and an app
that writes there either fails or gets reset by the OS with a notification. What still works is the ProgID plus
`OpenWithProgids`, which is what puts Prvw in "Open with" and in the Settings picker. `SettingKey::FileAssociations`
stays `Present`: the capability is reachable, only the surface differs. `file_types.rs`'s
`nothing_touches_the_user_choice` is the test that keeps a future contributor honest about it.

## Decision: a scale change rebuilds the dialog

**Decision:** `WM_DPICHANGED` posts `WM_REBUILD_FOR_DPI` to the dialog, which closes it and opens it again on the same
tab.

**Why:** every control's font, size, and position came from the old DPI, and the whole dialog is built from tables, so
rebuilding is both cheaper to write and more certainly correct than walking the tree resizing things. The post matters:
Windows keeps using the window after `WM_DPICHANGED` returns, so the rebuild has to happen on a later turn of the pump.

## Gotcha: clean up on `WM_NCDESTROY`, not `WM_DESTROY`

**Gotcha:** `WM_DESTROY` reaches the parent **before** its children. Deleting the font and the background brush there
pulls them out from under controls that still exist and can still send `WM_CTLCOLOR*`. `WM_NCDESTROY` is the last
message a window ever gets, after every child is gone, and that's where the cleanup lives.

## Gotcha: the accelerator target is the main window, not `msg.hwnd`

**Gotcha:** muda's own winit example passes `(*msg).hwnd` to `TranslateAcceleratorW`, which translates accelerators
against whatever has focus. Typing a comma into a settings field would open Settings. `platform::windows::msg_hook`
passes the main window instead, and takes dialog messages off the table first with `IsDialogMessageW`. That ordering is
why this dialog's keyboard works at all; read that module before touching the pump.

## Gotcha: the folder picker can't touch a window

**Gotcha:** "Browse…" runs `rfd::AsyncFileDialog::pick_folder` on a worker thread, because `IFileDialog::Show` is modal
and a modal on the event-loop thread freezes winit's pump. A worker thread can't write to an `HWND`, so the chosen path
comes back as `AppCommand::SetCustomDcpDir` and `app::executor` calls `sync_custom_dcp_dir` from the event loop's own
thread to fill the field in.

## Gotcha: `rfd`'s blocking API lies on an MTA thread

**Gotcha:** `rfd::FileDialog` (the blocking one) calls `CoInitializeEx(COINIT_APARTMENTTHREADED)`, gets
`RPC_E_CHANGED_MODE` on a thread that's already MTA, and turns the error into `None` — indistinguishable from the user
cancelling. Always `AsyncFileDialog`, here and in `open_dialog.rs`.

## What needs a real Windows box

Everything `dialog.rs` does. In rough order of how likely a surprise is:

- Whether `with_msg_hook` plus `IsDialogMessageW` composes cleanly. We found no other Rust project doing this.
- Whether `CreateDialogIndirectParamW` with a zero-control template and a later `SetWindowPos` gives the right frame.
- How much `WM_CTLCOLOR*` has to cover before the dark theme looks finished. The `uxtheme` ordinals themselves are the
  About box's problem too, so one Windows session checks both.
- Whether `ScrollWindowEx` with `SW_SCROLLCHILDREN` scrolls the RAW page cleanly, or leaves trails under a trackbar.
- Whether the `WM_DPICHANGED` rebuild is quick enough to look like a resize rather than a flicker.
