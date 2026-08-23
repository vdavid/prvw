# Prvw on Windows: the native chrome

Status: written 2026-08-23. None of the Windows chrome is built. The one piece that landed early is the bare `S`
slideshow shortcut, which is cross-platform and lives in `input::key_to_command`.

This spec answers one question: what should Prvw's Windows chrome look like, and what should we build it with. It covers
the menu bar, the settings surface, browse mode, onboarding, and about. It does not cover the image window, which stays
byte-for-byte the same on every platform.

It implements decision 1b in [cross-platform-plan.md](cross-platform-plan.md) and it is the design input for milestones
M1 (menus and accelerators), M4 (settings), M5 (browse mode), and M6 (onboarding and about). The scope is the 79
`Missing` Windows entries in [../parity.md](../parity.md).

Every claim about a Windows API, a crate version, or a control's behavior in this document was checked against a live
source on 2026-08-23. The end of each section says what could not be checked, and there is a collected list under "What
we could not verify". **There is no Windows machine available**, so nothing here has been run.

## The decision this implements

David, 2026-08-23: "the main window is pretty custom so it should probably be the same on all OSes, but for Settings,
menus, browsing, etc., I want to pursue a native feel on each OS, not being the same across platforms. Unlikely that
someone uses Prvw on both macOS and Windows, and even if they do, they know how each OS works and feels, and want their
apps to blend into that understanding. So make the Windows version very Windows-like."

Three things follow, and they are the standard the rest of this document is written against.

- **Don't port the macOS design.** Where a Windows convention differs, the Windows convention wins. A reviewer holding
  two screenshots side by side and finding different layouts is looking at the design working.
- **Parity is features reachable, never layouts matching.** The registries in `apps/desktop/src/parity/` already model
  this: `SettingKey` names the job (`Toggle`, `Slider`, `Choice`, `Path`, `Custom`) and each platform picks its own
  widget. Nothing below asks that separation to bend.
- **ACDSee 2.41 is the lineage, and it points the right way.** `AGENTS.md` names it as the model. It was a Win32 app
  with a conventional menu bar, a tabbed options dialog, and a tree-plus-thumbnails browser. That idiom is still the
  native one for a Windows image viewer, and this design leans into it rather than arguing with it. The one place we
  depart is dark mode, which did not exist in 1997 and does now.

## The technology choice

**Recommendation: Win32 common controls (comctl32 v6) through the `windows` crate, with `muda` for menus and `rfd` for
file dialogs.**

The single strongest argument: it is the only option that fits in a viewer whose entire premise is opening an image in
about 600 ms. WinUI 3 costs 40 to 90 MB of DLLs beside the exe or a 111 MB prerequisite install, and Microsoft's own
Rust benchmark for it reports 160 ms to first window and a 109.5 MB working set. That is the whole time budget spent
before we have read a file. Everything else about the comparison is secondary to that number.

The second argument is availability. There is no usable Rust binding for WinUI 3 today, and Microsoft's own position is
why: `Microsoft.UI.Xaml` was removed from the `windows` crate as "largely unusable" and "designed for C# developers",
and the experimental `windows-app-rs` projection was archived on 2022-08-24 with the note that "the Windows App SDK in
its current form is too heavily tied to .NET and Visual Studio to be practically usable with other languages and
toolchains". Choosing WinUI 3 means generating and maintaining our own bindings from `Microsoft.UI.Xaml.winmd`,
hand-implementing `IXamlMetadataProvider`, hand-declaring `ContentPreTranslateMessage`, and solving the pre-translate
collision with winit's message pump, all of it new ground with no prior art we could find.

### What we build on

- **`windows` 0.62.2** (published 2025-10-06, MSRV 1.82, MIT or Apache-2.0), feature-gated to the namespaces we touch:
  `Win32_UI_Controls`, `Win32_UI_WindowsAndMessaging`, `Win32_UI_Shell`, `Win32_Graphics_Dwm`,
  `Win32_UI_Controls_Dialogs`, `Win32_System_Com`, and a handful more. Since release 69 the crate uses `raw-dylib`
  through `windows-link`, so there are no import libraries to download and the disk footprint of the dependency is
  small.
- **`windows`, not `windows-sys`.** `windows-sys` is the right call for a leaf crate that must not inflate downstream
  build graphs, which Prvw is not. We need COM: `IFileOpenDialog`, `IShellItem`, and `IShellItemImageFactory` are
  painful with hand-rolled vtables and pleasant with the real projection.
- **`muda` 0.19.3** (published 2026-06-17) for the menu bar and the context menu, which the app already depends on for
  macOS. `Menu::init_for_hwnd_with_theme` takes any HWND including winit's, installs its own `SetWindowSubclass` to
  catch `WM_COMMAND`, and introduces no nested loop. `menu::poll_menu_event` keeps working unchanged.
- **`rfd` 0.17.2** (published 2026-01-12) for the file and folder pickers, always through `AsyncFileDialog`.
- **An embedded application manifest** carrying two things in one file: the `Microsoft.Windows.Common-Controls` version
  `6.0.0.0` dependency, without which we silently get comctl32 v5 (unthemed, no `SysLink` for the about box's
  hyperlinks, and none of the Explorer theming browse mode leans on), and `<dpiAwareness>PerMonitorV2</dpiAwareness>`.
  Both have to be in the manifest rather than set by an API call, because they apply before any HWND exists. Use the
  `embed-manifest` crate from `build.rs`.

### Why not the alternatives

- **WinUI 3 / Windows App SDK 2.4.0.** Technically the only path to a genuinely Windows-11-native look, and XAML Islands
  (`DesktopWindowXamlSource`) has been non-experimental since Windows App SDK 1.4, with an official unpackaged Win32 C++
  sample. It clears our Windows 10 22H2 floor easily (the SDK minimum is still 1809). It fails on size, on startup cost,
  and on the missing Rust binding, as above. It also needs `ContentPreTranslateMessage` called between `GetMessage` and
  dispatch, which is inside winit's loop; winit's `with_msg_hook` is the only injection point and we need that slot for
  accelerators.
- **WPF or WinForms through .NET.** Hosting .NET in a Rust process is genuinely solved (`netcorehost` 0.22.0, actively
  maintained). Everything else is worse than WinUI 3: the .NET 10 Windows Desktop Runtime is a 57.2 MiB prerequisite,
  self-contained WPF is around 150 MB, WPF supports neither trimming nor NativeAOT, and `Window.ShowDialog` and
  `Form.ShowDialog` both spin nested modal loops. It would also add a second language and MSBuild to a repo whose macOS
  chrome is objc2. Off the table.
- **Drawing the chrome ourselves in wgpu.** This is the option nobody asked for, and it is worth naming because it is
  genuinely tempting: one look on both platforms, no comctl32 theming problems, and it sidesteps the dark-mode mess
  described below entirely. It is also exactly what decision 1b rejected. It would make Prvw an app that renders its own
  idea of a settings window on top of Windows rather than an app that feels made for Windows, and browse mode in
  particular would stop looking like Explorer, which is the one comparison every Windows user will make. Rejected on the
  decision, not on the technology.
- **`native-windows-gui`.** Wraps nearly the whole comctl32 control set with a derive-macro API and is genuinely light.
  Its last release is 1.0.13 from 2022-09-05 and the author's own framing is "3rd and final version… the backlog is
  empty, and it will most likely stay that way". No dark-mode story. Read it as a reference implementation; don't depend
  on it.
- **`winsafe` 0.0.28.** Actively maintained safe Win32 bindings with a real control set. Its `gui` module wants to own
  the window, which conflicts with winit owning ours, and it is still `0.0.x` after years. The wrapper half is usable a
  la carte if we ever want it; the framework half is not a fit.

### The one rule: no nested message loop, ever

This is the constraint that shapes every surface below, and it is the Windows form of a rule Prvw already lives by on
macOS. `runModalForWindow` inside winit's callbacks segfaults on autorelease pool cleanup; a Win32 modal loop does not
segfault, it just freezes winit's pump. Different cause, same rule.

Raymond Chen's framing is the one to internalize: modality has two independent axes. _Code_ modality is a nested message
loop that runs until an exit condition. _UI_ modality is only "the user can't touch the other window", and
`EnableWindow(hwnd, FALSE)` gets you that with no loop at all. We want UI modality where it helps and code modality
nowhere.

What blocks, verified against Microsoft's own docs:

- `DialogBoxParam` "disables the owner window, and starts its own message loop". Never call it.
- `IFileOpenDialog::Show` and `IFileSaveDialog::Show` block, and `IModalWindow` has no modeless mode and no injection
  point. A separate thread is the only escape.
- `TaskDialogIndirect` blocks.
- `PropertySheet` is modal unless you pass `PSH_MODELESS`.
- `TrackPopupMenu` blocks, `WM_ENTERMENULOOP` is literally documented as "a menu modal loop has been entered", and
  `WM_ENTERSIZEMOVE` is the same for a title-bar drag. These are system-owned and unavoidable. Every Win32 app has them.
  Don't fight them, but do know the consequence: while a menu is down or the window is being dragged, winit's
  `about_to_wait` does not run, so `ControlFlow::WaitUntil` timers and `EventLoopProxy` user events stall. The slideshow
  timer pauses while a menu is open. That is native behavior.

What doesn't block:

- `CreateDialogParamW` creates a modeless dialog, runs no loop, and does not disable the owner. Its price is that "the
  message loop for the dialog box must call the `IsDialogMessage` function", or Tab, arrow keys, Enter, Esc, and
  mnemonics all stop working inside it.
- Ordinary child HWNDs need nothing at all.

**The integration point is `EventLoopBuilderExtWindows::with_msg_hook`**, which exists in winit 0.30.13 and is unchanged
in 0.31 beta. Its documented contract: "A callback to be executed before dispatching a win32 message to the window
procedure. Return true to disable winit's internal message dispatching." winit calls it right after
`PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE)` and before `TranslateMessage`/`DispatchMessageW`, with `hwnd = 0`, so the
hook sees every message on the thread, including ones for child HWNDs and modeless dialogs. The `*const c_void` is
really a `*mut MSG`.

**winit stores exactly one hook.** So accelerator translation and dialog-message handling share one closure, and the
order matters:

1. **Modeless dialogs first.** Walk the set of open dialog HWNDs; for a message where
   `msg.hwnd == hdlg || IsChild(hdlg, msg.hwnd)`, return `IsDialogMessageW(hdlg, msg) != 0` and stop. A message
   `IsDialogMessage` handles must not also reach `TranslateMessage`, which is why this returns rather than falling
   through.
2. **Menu accelerators second**, `TranslateAcceleratorW(main_hwnd, haccel, msg)`.

Note the deviation from muda's own winit example, which passes `(*msg).hwnd` as the accelerator target. That would
translate accelerators against whatever window has focus, so typing a comma into a settings field would open the
settings dialog. **Pass the main window's HWND**, and let step 1 above take dialog messages off the table first.

Keep the dialog set as a set rather than a stack: modeless dialogs are concurrent, not nested. Respect controls that
claim a key with `DLGC_WANTALLKEYS` or `DLGC_WANTMESSAGE`; `IsDialogMessage` does that itself, which is another reason
to use it rather than hand-rolling Tab handling.

Two more integration facts worth writing into `menu/CLAUDE.md` when this lands:

- **muda's accelerators do not work without this hook.** muda's docs say so directly: "On Windows, accelerators don't
  work unless the win32 message loop calls `TranslateAcceleratorW`." Without it, Ctrl+C, Ctrl+P, Ctrl+= and Ctrl+-, and
  Ctrl+, all silently do nothing. This is M1 step 9 and it is not optional.
- **`rfd`'s blocking API silently returns `None` when the calling thread is MTA.** Its `init_com` calls
  `CoInitializeEx(COINIT_APARTMENTTHREADED)`, gets `RPC_E_CHANGED_MODE` on an MTA thread, and the public API turns the
  error into `None`, which is indistinguishable from "the user cancelled". winit's main thread is `OleInitialize`d (STA)
  so we are fine in practice, but the failure mode is nasty enough to document.

### What we could not verify about the technology

- No 2025 or 2026 authoritative measurement of a self-contained unpackaged WinUI 3 app's on-disk size exists. The 40 to
  90 MB range is triangulated from a 2022 measurement of 44.3 MiB, a 2024 report of about 30 MB, and Microsoft's own
  31-file runtime allowlist. If this number would change the decision, build a hello-world and measure it.
- We found no existing Rust project combining `with_msg_hook` with `IsDialogMessageW`. The only real-world hook usage in
  the wild is muda's `TranslateAcceleratorW` example. Budget time for that integration being new ground.
- Whether `SetMenu`-ing a menu bar onto a winit window confuses winit's client-area or DPI math. Plausible, untested,
  and worth checking in the first hour of M1 step 9 rather than at the end.
- Whether wgpu keeps rendering during a system-owned nested modal loop (a menu drop-down, a title-bar drag). Reading
  winit 0.30.13, the `WM_PAINT` arm emits `RedrawRequested` synchronously from the window procedure and a nested loop
  still calls `DispatchMessage`, so `InvalidateRect`-driven repaints should keep flowing. Worth 20 minutes of empirical
  checking on the Windows box.

## The menu bar

A real Win32 menu bar, attached to winit's HWND with `muda::Menu::init_for_hwnd_with_theme`. Not a hamburger, not a
command bar, not a custom-drawn strip. The menu bar is what a Windows image viewer has, it is what ACDSee had, and it is
what IrfanView, XnView, and FastStone still have.

### The menus

Seven top-level menus, in this order: File, Edit, View, Navigate, Slideshow, Tools, Help.

- **File**: Open… (Ctrl+O), separator, Print… (Ctrl+P), separator, Exit. "Show in File Explorer" joins this menu when
  the reveal pair lands (see the decisions at the end), which is after M1.
- **Edit**: Copy image (Ctrl+C). Only Copy, same reasoning as macOS: Cut, Paste, and Select all make no sense in a
  viewer and showing them disabled looks broken.
- **View**: Zoom in (Ctrl++), Zoom out (Ctrl+-), separator, Actual size (Ctrl+0), Fit to window, Auto-fit window,
  Enlarge small images, separator, ICC color management (Ctrl+Shift+I), Color match display (Ctrl+Shift+C), Relative
  colorimetric (Ctrl+Shift+R), separator, Histogram (H), Exif info (E), Sort by submenu (Name, Date, File type),
  separator, Fullscreen (F11), separator, Refresh (F5).
- **Navigate**: Image browser (Enter), separator, Previous (Left arrow), Next (Right arrow), separator, Go to first
  (Home), Go to last (End), separator, Loop navigation.
- **Slideshow**: Start slideshow (S), separator, Increase speed (]), Decrease speed ([).
- **Tools**: Settings… (Ctrl+,).
- **Help**: About Prvw.

### Where the macOS app menu goes

There is no app menu on Windows, so its six items scatter:

- **About Prvw** goes to Help, as its only item and therefore its last item. That is the Windows convention and it has
  been since Windows 3.0. No accelerator.
- **Settings…** goes to Tools. A one-item Tools menu is unremarkable on Windows and it is the first place a Windows user
  looks; the living Windows image viewers put settings under Tools (XnView, ACDSee) or a top-level Options menu
  (IrfanView, FastStone). Ctrl+, is now a real Windows convention, not a macOS import: Visual Studio, VS Code, Windows
  Terminal, and Edge all use it. We keep the shared label "Settings…" rather than renaming it to the older "Options…",
  because modern Windows says Settings and because `MenuItemKey::label` is a shared string by design.
- **Quit Prvw** becomes **Exit**, at the bottom of File, with no accelerator (Alt+F4 belongs to the window, not to us,
  and Notepad shows no accelerator there either). The registry already permits this: its docs say that for the items the
  toolkit builds, the label "is the name we call them", and `PredefinedMenuItem::quit(Some("Exit"))` supplies the real
  title.
- **Hide Prvw, Hide others, Show all** stay `NotApplicable`, with the reason already in the registry.

### Close window becomes NotApplicable

`MenuItemKey::CloseWindow` is `Missing` on Windows today. It should be `NotApplicable`, with this reason:

> Prvw has one window on Windows, and a Windows app with no windows is an invisible process rather than a running app.
> Closing that window is exiting, which File → Exit already does.

That is a genuine platform fact, not a shrug: it is the same fact that makes M1 step 1's no-argument launch a bug on
Windows and merely a design choice on macOS.

### Accelerators, and where they differ from macOS

The rule is Ctrl where macOS uses Cmd, and then three deliberate Windows wins:

- **Fullscreen becomes a real F11 accelerator.** On macOS it is a cosmetic hint in the item's title, because a
  bare-letter menu equivalent is app-global and would hijack typing into settings fields. F11 is not a typing key, so
  Windows gets the real thing. Bare `f` keeps working through `input`.
- **Refresh becomes F5.** It has no accelerator on macOS. F5 means refresh in Explorer and in every browser, and Prvw
  should honor that.
- **Print stays Ctrl+P, Copy stays Ctrl+C, Open is Ctrl+O.** No surprises.

Kept as cosmetic hints, handled by `input` exactly as on macOS: S (Start slideshow), H (Histogram), E (Exif info), L
(Loop navigation), the arrow keys, `]` and `[`, and Enter for the image browser. The reason is the same on both
platforms, and on Windows it is sharper: the hook order above means a real accelerator table entry would fire even when
a settings field has focus if we ever got the ordering wrong. Cosmetic hints have no such failure mode.

**Slideshow is bare `S`, and Ctrl+S stays bound to nothing.** Starting a slideshow is a viewer-state toggle, and every
other one in Prvw is a bare single letter: `h` histogram, `e` Exif, `l` loop, `f` fullscreen, `[` and `]` speed, `;` and
`'` navigate. Ctrl+S would be the only modified shortcut in that family, and it would be the one place a Windows user's
Save reflex lands on something. Leaving Ctrl+S unbound means the reflex does nothing, which is the right answer in an
app that cannot save. `S` is implemented in `input::key_to_command` on every platform; the Windows menu shows it in the
item's shortcut column, exactly like `H` and `E`.

**macOS reaches the same answer, so ⌘S is gone from that item too.** The Save reflex is a Mac reflex as much as a
Windows one, and the argument doesn't get weaker for being made about Command. So `SlideshowToggle` carries no key
equivalent on either platform, and `MenuItemKey::hint` paints `S` into the macOS title the way `Fullscreen` shows `F`.

**Ctrl+0 stays Actual size**, matching macOS and matching browsers, even though the bare `0` key means Fit to window and
the bare `1` means Actual size. That inconsistency is inherited from the macOS design and this is not the place to fix
it.

### Two Windows-only presentation details the registry has to accommodate

Both are decoration, not naming, so `MenuItemKey::label` stays the single shared string and the Windows menu builder
decorates it. A new `menu/windows.rs` table keyed by `MenuItemKey` owns the decoration and the `ACCEL` entries.

- **Shortcut text is tab-separated, not space-padded.** Windows right-aligns the accelerator column when the item string
  contains a tab: `"Fullscreen\tF11"`. `MenuItemKey::title` composes `label + hint` where `hint` is space-padded
  (`"Fullscreen        F"`), which is a macOS presentation choice. Windows builds from `label` plus its own table.
- **Mnemonics need ampersands.** `&File`, `&Open…`, `E&xit`. Alt+F then O is how a large number of Windows users
  actually drive a menu bar, and omitting them is one of the clearest tells that an app was ported rather than written
  for Windows. Mnemonics must be unique within each menu; the assignment is the Windows builder's business.

The parity audit compares by key, not by rendered string, so neither of these weakens the guarantee. The registry's own
`CLAUDE.md` already flags keyboard shortcuts as something layer 1 does not cover, and this table is where that gets
fixed for Windows.

### Menu bar and fullscreen

**The menu bar is always visible in windowed mode and gone in fullscreen. There is no auto-hide and no setting for it in
v1.** `SetMenu(hwnd, null)` on entering fullscreen, restoring it on exit. Fullscreen is where the image really is 99% of
the app, and no Windows app shows a menu bar there.

The tempting third option is Explorer's classic behavior: hide the bar and reveal it on Alt. It is a real Windows
pattern, and it is rejected on four counts.

- **Alt-reveal is discoverable only by accident.** A user who does not already know the gesture sees an app with no
  menus.
- **The menu is the only mouse path to most features until M5 ships browse mode.** Hiding it by default hides the app.
- **F11 is already the escape hatch** for anyone who wants the chrome gone.
- **A setting costs far more than it buys.** A `SettingKey` with no macOS counterpart, a parity entry, a visible/hidden
  state machine that has to compose with fullscreen, and the bugs that come with it, all for about 20 logical pixels
  above a letterboxed image.

## The settings surface

**Recommendation: a modeless, non-blocking dialog hosting a `SysTabControl32` with six tabs and a single Close button.
The main window stays live and interactive behind it.**

### Why this shape

- **Not a modal property sheet.** Modal means a nested loop, which is the one rule.
- **Not the `PropertySheet` API even with `PSH_MODELESS`.** `PSH_MODELESS` does return immediately, and it is a
  legitimate option, but it brings its own protocol: `PSM_ISDIALOGMESSAGE` instead of `IsDialogMessage`,
  `PSM_GETCURRENTPAGEHWND` returning NULL as the "user closed it" signal, an incompatibility with `PSH_AEROWIZARD`, and
  a built-in OK/Cancel/Apply button bar we would have to suppress. A plain modeless dialog with a tab control looks
  identical to a user and has none of that.
- **Not a Windows 11 Settings-style navigation pane.** That look is `NavigationView` plus `SettingsCard`, which is a
  Community Toolkit control for WinUI, not an inbox control and not something comctl32 has. Reproducing it in Win32
  means own-drawing rounded cards with hover elevation and Fluent icons, which is writing a small layout engine, and we
  found no open-source project that has done it. Microsoft's own answer to this exact question was "use XAML Islands".
  Tabs are the comctl32-native answer and they are what ACDSee had.
- **Not modal at all, and no OK/Cancel/Apply.** Three reasons. Settings apply immediately through `AppCommand` on every
  platform, and re-introducing OK/Cancel would need a transaction layer that does not exist. The RAW tab's seven sliders
  are live-tuning controls whose entire point is watching the image change as you drag, which a modal dialog makes
  impossible. And Windows itself has moved this way: Windows 11 Settings applies immediately and has no Apply button.
  So: modeless, owner not disabled, one Close button bottom-right, Esc closes.

### The dialog

- Roughly 560 by 480 logical pixels at 96 DPI, not resizable, centered on the main window on first open.
- Tabs across the top, in the macOS sidebar order so the two builds group settings the same way: General, Zoom, Color,
  RAW, Slideshow, File associations.
- Each tab's page is a child window with `WS_EX_CONTROLPARENT` so `IsDialogMessage` walks into it for Tab navigation.
  Pages are built once and shown or hidden on `TCN_SELCHANGE`, matching the macOS retained-mode `setHidden:` pattern
  rather than creating and destroying.
- Close button bottom-right, `BS_DEFPUSHBUTTON`.
- Font from `SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, ..., dpi)`, taking `lfMessageFont`, applied to every
  control with `WM_SETFONT`. Not `GetStockObject(DEFAULT_GUI_FONT)`, which Microsoft deprecates for this. This gets the
  right font and size on both Windows 10 and 11 with no branching (see "Windows 10 versus Windows 11" below).
- DPI: reposition and refont on `WM_DPICHANGED`, using the suggested rect verbatim.

### Build the controls in code, not from an .rc template

Windows scales `CreateDialog` templates automatically under Per-Monitor v2, which is a real argument for using an `.rc`
file. We should not, and the reason is the parity harness. `settings::widgets::make_setting_row` takes a `SettingKey`
and reads the row's title from it, which is exactly what makes it impossible to put a settings row on screen without
registering it. A dialog template hardcodes its label strings, so a template-built Windows settings dialog would sever
the one link M0.5 exists to create.

So: build controls programmatically from `SettingKey`, and pay for it with a small layout helper
(`settings/windows/layout.rs`, a vertical stacker that places rows at `MulDiv(y, dpi, 96)`) shared by all six pages.
Call it 150 lines, against the several hundred a template would save, and it keeps the compile-time guarantee.

### Widget choices

- **`Toggle` is a checkbox** (`BUTTON`, `BS_AUTOCHECKBOX`), not a switch. comctl32 has no toggle switch at all, and a
  checkbox is what "native" means here. This is one of the places the two platforms visibly differ (macOS uses
  `NSSwitch`) and that is the design working.
- **`Slider` is a trackbar** (`msctls_trackbar32`, `TBS_HORZ | TBS_AUTOTICKS`) with a right-aligned read-only static
  showing the current numeric value, updated on `WM_HSCROLL`.
- **`Path` is a read-only edit control plus a "Browse…" button and a "Clear" button.** Browse opens a folder picker
  through `rfd::AsyncFileDialog::pick_folder().set_parent(&window)`, never the blocking variant.
- **Descriptions are a grey static under the control**, `GetSysColor(COLOR_GRAYTEXT)`, indented to align with the
  checkbox label rather than the box. This is the Windows 11 Settings card subtitle idea without the card, and it keeps
  the macOS row's information density. **The copy is rewritten, not translated**: macOS descriptions talk about macOS
  hardware ("wide-gamut (P3) screens like MacBooks"), and the registry deliberately keeps `description` a per-platform
  argument for exactly this.
- **Sections inside a page are group boxes** (`BUTTON`, `BS_GROUPBOX`), which is the Windows-native form of the macOS
  bold section header.
- **No combobox, no spin control, no listbox, no date picker anywhere in this dialog.** This is not an aesthetic
  preference: Windows 11 restyled buttons, checkboxes, radio buttons, scrollbars, and edit fields, and did _not_ restyle
  comboboxes, spinners, listboxes, or the up-down control, which is why Win32 settings dialogs look dated on Windows 11.
  Restricting ourselves to checkboxes, trackbars, buttons, statics, group boxes, and one tab control means the dialog
  reads as current on Windows 11 and correct on Windows 10, from one implementation. We get this almost free because the
  only `Choice` setting is menu-only.

### Every setting, and where it lands

Forty `SettingKey` entries. All of them are reachable on Windows under this design, so the settings registry goes from
`0 done / 1 not applicable / 39 missing` to `39 done / 1 not applicable / 0 missing`.

**General tab, four keys.** `AutoUpdate`, `ScrollToZoom`, and `PreloadNeighbors` are checkboxes. `TitleBar` stays
`NotApplicable` with the reason already in the registry: a Win32 client area starts below the caption, so there is no
title bar overlapping the image and nothing to reserve space for.

**Zoom tab, two keys.** `AutoFitWindow` and `EnlargeSmallImages`, both checkboxes. As on macOS, "Enlarge small images"
is deliberately _not_ disabled by "Auto-fit window": auto-fit is inert in fullscreen, where enlarge still governs.

**Color tab, three keys.** `IccColorManagement`, `ColorMatchDisplay`, and `RelativeColorimetric`, all checkboxes. The
cross-dependency carries over: unchecking ICC color management calls `EnableWindow(hwnd, FALSE)` on the other two. The
description copy needs rewriting for Windows display hardware, and it should mention Windows 11 Auto Color Management,
which is on by default on supported displays and can double-transform what a color-aware app does. That is listed as a
risk in the cross-platform plan and it is the kind of thing a user will read the settings copy to understand.

**RAW tab, 23 keys**, in eight group boxes matching the macOS panel's section headers exactly:

- Sensor corrections (DNG only): `RawDngOpcodeList1`, `RawDngOpcodeList2`, `RawDngOpcodeList3`.
- Color: `RawBaselineExposure`, then `RawBaselineExposureOffset` as a trackbar indented under it, `RawDcpHueSatMap`,
  `RawDcpLookTable`, `RawSaturationBoost`, then `RawSaturationAmount` as a trackbar under it.
- Tone: `RawHighlightRecovery`, `RawDefaultToneCurve`, then `RawToneMidtoneAnchor` as a trackbar under it,
  `RawDcpToneCurve`.
- Detail: `RawClarity`, then `RawClarityRadius` and `RawClarityAmount` as trackbars under it, `RawCaptureSharpening`,
  then `RawSharpenAmount` as a trackbar under it.
- Denoise: `RawChromaDenoise`.
- Geometry: `RawLensCorrection`.
- Output: `RawHdrOutput`, then `RawHdrGain` as a trackbar under it.
- DCP profile: `CustomDcpDir`, as the read-only edit plus Browse and Clear.

Plus the Reset button at the bottom, matching macOS.

This page is far too tall for a 480-pixel dialog, so **the RAW page's content lives in a scrolling child container**
(`WS_VSCROLL`, repositioning the container on `WM_VSCROLL`). Roughly 100 lines, and it is the only page that needs it.
The alternative, splitting RAW across two tabs, is permitted by decision 1b but it would put the same feature in two
places for a reason that is purely about pixel height, and a Windows user scrolling an options page is completely
ordinary.

**Slideshow tab, three keys.** `SlideshowSeconds` as a trackbar, `SlideshowCrossfade` and `SlideshowLoop` as checkboxes.

**File associations tab, one key.** `FileAssociations` is `Control::Custom`, and this is the one page that cannot mirror
macOS at all, because **Windows removed programmatic default-handler setting in Windows 10 20H2**. Registry edits,
`assoc`, and `ftype` are ignored. So the macOS panel's 16 per-format toggles become three things:

1. A read-only list of the extensions Prvw registers (a static, or a report-mode listview if the list grows).
2. A "Register Prvw's file types" button, which writes the ProgID under `HKCU\Software\Classes\Applications\prvw.exe`
   plus the `OpenWithProgids` entries. This is what makes Prvw appear in Explorer's "Open with" list. The installer does
   it too; the button is the repair path.
3. A "Open Windows default apps settings" button, which launches `ms-settings:defaultapps`, plus one line of copy
   explaining that Windows itself owns this choice now.

**The key stays `Present`, not `NotApplicable`.** The capability, choosing which file types open in Prvw, is reachable;
the surface is different because the OS took the switch away. `NotApplicable` is for a setting that is meaningless on a
platform, and this one is not.

**Menu only, four keys.** `HistogramVisible`, `ExifVisible`, `LoopNavigation`, and `SortBy` have no dialog rows on
either platform. They are `Present` on Windows through the View and Navigate menus. `SettingKey` coverage asks "is it
exposed anywhere on this platform", and `SettingKey::panel_coverage` already filters `Panel::None` out before the window
audit compares, so this needs no special handling.

## Browse mode

**Recommendation: a folder tree and a thumbnail grid as child HWNDs of the winit window, `SysTreeView32` on the left and
a virtual `SysListView32` in icon mode on the right, with a hand-written splitter between them and a status bar along
the bottom.** This is ACDSee's shape and Explorer's shape, and it is what a Windows user will compare it to.

The macOS structure carries over unchanged where it should: the two screens **swap** rather than composite, and all four
pure cores (`grid_model`, `grid_scheduler`, `thumbnail_cache`, `tree_model`) come along untouched. This milestone is the
shell around them.

### The controls

- **The tree** is `SysTreeView32` with
  `TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_SHOWSELALWAYS | TVS_TRACKSELECT`, `TVS_EX_DOUBLEBUFFER`, and,
  importantly, `SetWindowTheme(hwnd, L"Explorer", nullptr)`. That last call is what turns a Windows-95-looking treeview
  with plus and minus boxes into Explorer's chevron glyphs and hot-tracking. Without it the whole browser looks two
  decades old. It is also the single highest-value line in this milestone.
- **The grid** is `SysListView32` with `LVS_ICON | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_OWNERDATA`,
  `LVS_EX_DOUBLEBUFFER`, and the same `SetWindowTheme(hwnd, L"Explorer")`. `LVS_OWNERDATA` makes it virtual, so it asks
  us for item text and image index through `LVN_GETDISPINFO` rather than us pushing thousands of items into it.
  Thumbnails go into an `HIMAGELIST` sized to the cell.
- **The splitter is ours to write.** Win32 has no splitter control. It is a thin child window handling `WM_SETCURSOR`
  (`IDC_SIZEWE`), `WM_LBUTTONDOWN` with `SetCapture`, `WM_MOUSEMOVE`, and `WM_LBUTTONUP`. Call it 80 lines. Default the
  tree pane to about 240 logical pixels, matching macOS.
- **A status bar** (`msctls_statusbar32`) across the bottom, with the image count in the current folder, the selected
  file's name, and its dimensions. **Build it, Windows-only.** A status bar is a Windows idiom that ACDSee had, it costs
  almost nothing, and it gives the browser somewhere to say "Loading …" that is not an overlay. Its parity coverage is
  settled under "The browse-mode status bar" below.

### How it composes with the wgpu surface

The macOS constraint carries over: a transparent GPU pixel occludes what is behind it, so the native UI has to sit in
front of the surface, not behind it, and the two screens hide-one-show-the-other rather than compositing. On DXGI the
same conclusion holds for a different reason. wgpu's DX12 backend uses a flip-model swapchain on the HWND, and
flip-model presentation composes as its own DWM visual rather than respecting GDI's child-window clipping the way the
old blt model did.

The design is safe by construction rather than by luck:

- **Build the whole browse UI inside one child container HWND that covers the entire client area**, and show or hide
  that one window. Same shape as macOS's single `prvw.browser_split` sibling view.
- **Set `WS_CLIPCHILDREN` on the winit window** so ordinary GDI painting does not fight the controls.
- **Stop presenting while browse mode is up.** macOS already does the equivalent: it paints one black frame, hides the
  Metal layer, and requests no further redraws, so the GPU goes idle. On Windows the same render-on-demand behavior
  means no `Present` call happens while the browser is visible, so there is nothing to paint over the controls. The
  container repaints itself opaquely and owns the client area.

**If child HWNDs over a flip-model swapchain turn out to be unreliable in practice**, the fallback is to make browse
mode a separate top-level window that the main window hands off to. That is also a perfectly Windows-native shape (it is
what a lot of viewers do), it costs a little state plumbing, and it removes the z-order question entirely. Decide this
in the first days of M5 with a spike, not at the end.

### Drive and folder enumeration

Windows has drive letters and no `/Volumes`, so `tree_model::enumerate_roots` needs a real Windows implementation.
`build_roots` is pure and takes whatever the platform hands it, so the seam already exists.

**The roots, in this order:**

1. **Known folders**, through `SHGetKnownFolderPath`: Pictures (`FOLDERID_Pictures`), Desktop (`FOLDERID_Desktop`),
   Downloads (`FOLDERID_Downloads`). Pictures first, because this is a photo viewer and that is where photos are. These
   replace macOS's single Home root, and they are what Explorer's navigation pane leads with.
2. **Every drive**, from `GetLogicalDrives()`, labelled the way Explorer labels them: the volume label from
   `GetVolumeInformationW` plus the letter in parentheses, so `Photos (D:)`, falling back to the drive-type name when
   there is no label, so `Local Disk (C:)`. Icons by `GetDriveTypeW`: fixed, removable, remote, and CD-ROM each get the
   right shell icon through `SHGetFileInfoW` with `SHGFI_SYSICONINDEX`.

**`GetVolumeInformationW` can block for tens of seconds on a disconnected network drive.** `GetLogicalDrives` is just a
bitmask and is instant, so enumerate the letters on the main thread, show the bare letter immediately, and fetch labels
on the existing `TreeScanner` thread. This is the same discipline the macOS tree already enforces, for the same reason:
a stale SMB mount must never freeze the UI. `tree_model`'s `ChildCache` state machine and the 1-second "Loading…"
overlay carry over unchanged.

**Hidden entries.** `tree_model.rs:202` tests for a leading dot. On Windows it becomes
`FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM`, both skipped unconditionally. We deliberately do not read Explorer's
"show hidden files" setting: a photo browser showing `AppData` and `System Volume Information` is noise, and skipping
unconditionally matches both Explorer's default and the macOS behavior.

**No network enumeration in v1.** Mapped network drives appear through `GetLogicalDrives` anyway. Enumerating network
neighborhoods with `WNetOpenEnum` is slow, blocking, and a source of hangs, and no one browsing photos needs it.

**Long paths.** The verbatim (`\\?\`) and case-insensitive path work from M1 step 10 lands here in `reveal_path_chain`.
`longPathAware` goes in the same manifest as the comctl32 dependency.

### Layout, keyboard, and how it differs from macOS

- **No sidebar vibrancy, no rounded gallery surface, no insets for traffic lights.** The tree fills its pane on
  `COLOR_WINDOW`; the grid fills its pane on `COLOR_WINDOW`. Windows does not have the "content floating on a material"
  idiom that the macOS browser uses, and imitating it would look wrong.
- **Arrow keys stay native**, exactly as on macOS: the treeview and listview handle Up, Down, Left, Right, Page Up, Page
  Down, and type-select themselves. We subclass only to intercept Tab, Enter, and Esc.
- **Tab moves focus between panes, Enter opens, Esc returns to the image.** Unchanged from macOS, and all three are
  already in `browser::browse_keydown_command`.
- **Backspace goes to the parent folder** in the tree. That is Explorer's convention and it is free here, because in
  browse mode Backspace has no other job (in image mode it means Previous).
- **Double-click opens.** Already the macOS behavior; on Windows it is also what a listview does by default.
- **The grid shows images only, never folders.** Explorer mixes them; Prvw does not, on either platform. The tree is the
  navigation model. Worth stating because it is the one place a Windows user might expect Explorer's behavior and will
  not get it.
- **A right-click menu over the grid** with Open, Copy image, and "Show in File Explorer". That last item ships as its
  own change alongside its macOS twin, "Reveal in Finder", rather than inside M1 or M5; the reasoning is under "Show in
  File Explorer and Reveal in Finder" below. Until it lands the grid's context menu carries Open and Copy image.
- **No thumbnail size slider in v1**, matching the macOS deferral. When it lands, use Explorer's own vocabulary as a
  View submenu (Extra large icons, Large icons, Medium icons, Small icons) rather than a slider, because that is the
  control Windows users already know. The "generate at `MAX_CELL_PT × 2`, downscale below it" rule already supports it
  with no regeneration.
- **No address bar or path box.** Explorer has one; the tree is Prvw's navigation model on both platforms, and adding a
  path box is a new surface with no macOS counterpart and no obvious owner.

### Thumbnails

M3 already decides this: `IShellItemImageFactory::GetImage`, mirroring the QuickLook worker's shape. Two Windows
specifics that belong in this design rather than in M3's notes:

- Microsoft's doc is explicit that "icon extraction can be time consuming and this method generally should not be called
  from a UI thread". The pattern is `SIIGBF_INCACHEONLY` on the UI thread for an instant answer from Explorer's own
  cache, falling back to a worker thread otherwise. That maps directly onto the existing scheduler, and it means a
  folder Explorer has already visited paints instantly.
- Every worker thread doing this needs `CoInitializeEx` and a matching `CoUninitialize`, and the apartment model decides
  whether `GetImage` blocks.

## Onboarding and about

**Recommendation: build no onboarding window on Windows at all, and build a small About dialog of our own.** This is the
place to argue for less, and the argument is strong.

### Why there is nothing to onboard

The macOS onboarding is a four-step checklist. Take the steps one at a time on Windows:

1. **"Install Prvw.app"** is always checked on macOS because running the binary means it is installed. On Windows the
   installer ran. There is nothing to say.
2. **"Set Prvw as your default image viewer"** cannot be done. Windows removed programmatic default-handler setting in
   20H2. The most an app can do is register its ProgIDs, which the installer already did, and then send the user to
   `ms-settings:defaultapps`. Meanwhile Windows itself already prompts, with "How do you want to open this file?", the
   first time the user double-clicks a JPEG after installing something new. An onboarding step here duplicates the OS's
   own flow and does it worse.
3. **"Move Prvw.app to /Applications"** is meaningless. The installer places the binary.
4. **"How to open images"** is one sentence.

So the honest Windows answer is a sentence and a link, and 2,349 lines of AppKit becomes close to nothing.

### What replaces it

**The no-argument launch shows an empty state inside the main window**, drawn with the existing `render/text.rs` in
wgpu. M1 step 1 has to build a defensible empty state anyway, because today `app.rs:3257` returns from `resumed()`
without building a window when `waiting_for_file` is true and the fallback is macOS-only onboarding, which off macOS
means an invisible process with no recovery. This design says: that empty state _is_ the Windows onboarding.

Its content, in order:

- The app icon.
- "Open an image to start" as the main line.
- "Press Ctrl+O, or drop an image here" as the secondary line.
- When Prvw is not the default handler for common image types, one more line: "Prvw isn't your default image viewer
  yet." with a "Set as default" affordance that launches `ms-settings:defaultapps`. When it is the default, this line is
  absent. Checking is a registry read of the `UserChoice` ProgID for a few extensions, and it is cheap enough to do on
  each empty-state paint.

No new toolkit surface, no new window, and it is reachable from the one place a Windows user will actually hit it.

**M6 therefore shrinks from one to two weeks to three or four days**, most of which is the about box. Note the two
dependencies this creates: M1 step 1 owns the empty state, so M6 becomes a polish pass over work M1 already did, and the
about box reuses M4's dark-mode and layout plumbing, so M6 has to follow M4 rather than float.

### The about box

**Our own small modeless dialog, built the same way the settings dialog is.** `CreateDialogParamW`-free: a plain
`WS_POPUP | WS_CAPTION | WS_SYSMENU` window with the app icon, "Prvw" as the heading, statics carrying the version, the
author credit, and the license line, two `SysLink` controls for getprvw.com and the license, and one Close button. It
joins the `with_msg_hook` dialog set like any other modeless dialog, so Tab and Esc work through `IsDialogMessageW`.

**Not a `TaskDialogIndirect`.** The argument is marginal cost, not taste. The settings surface already forces us to
build six tabs of dark-themed Win32 dialogs: the `SetPreferredAppMode` plumbing, the theme-name-per-control table, the
`lfMessageFont` handling, the DPI reposition, and the layout helper all exist whether or not About uses them. About is
then a handful of controls on top of machinery that is already paid for.

A task dialog would instead add a second dialog mechanism to the app, and the one flaw it has is exactly the thing worth
avoiding: **task dialogs do not follow dark mode.** A light box emerging from a dark app is one of the clearest tells
that an app was ported rather than written for Windows, and it is the tell right next to the app's name and version.
`TaskDialogIndirect` also blocks, so it would need its own thread, which is a second lifetime to reason about for a box
that says four lines.

Call it 150 lines against macOS's 252 plus its share of `ui_common`, and it is what a Windows user expects: a small
About box, correct at any DPI, v6-themed, and dark when the system is dark.

Implement it as `CommandKey::About` dispatching to that dialog rather than using muda's predefined item, so that
`MenuItemKey::About`'s registered command stays `Some(CommandKey::About)` and the audit keeps working.

## Windows 10 versus Windows 11

The floor is Windows 10 22H2 (build 19045) and both versions are full-fidelity targets. The good news is that almost
nothing needs a version branch.

**Single code path, no branch needed:**

- **Per-Monitor v2 DPI** has been available since Windows 10 1703, well below the floor. Declared in the manifest. Under
  PMv2 the system scales comctl32's theme-drawn assets and the non-client area for us; control positions, sizes, and
  fonts stay ours to handle on `WM_DPICHANGED`. Use `GetSystemMetricsForDpi`, `AdjustWindowRectExForDpi`, and
  `SystemParametersInfoForDpi` rather than their unsuffixed forms. One gotcha that could bite the browse-mode child
  windows: every HWND in a window tree must share one DPI awareness mode, and a mismatched `SetParent` on 1703 and later
  either fails with `ERROR_INVALID_STATE` or force-resets the whole process's awareness.
- **The system font.** `SystemParametersInfo(SPI_GETNONCLIENTMETRICS)` returns "Segoe UI" on Windows 11, not "Segoe UI
  Variable"; the Variable switch is a XAML-layer thing, which Microsoft's own typography guidance says outright and
  which Firefox and VS Code both closed as not-planned. So reading `lfMessageFont` gives the correct font on both
  versions with no branch. (We could not verify whether Segoe UI Variable ever shipped to Windows 10, so do not assume
  it is present on 22H2.)
- **Rounded window corners.** Windows 11 auto-rounds any top-level window with `WS_THICKFRAME | WS_CAPTION`, so the main
  window and the settings dialog get it free and Windows 10 correctly gets nothing.
- **Common-control look.** `SysTreeView32` and `SysListView32` render essentially identically on both versions, which is
  exactly the pair browse mode leans on. The controls Windows 11 left un-restyled are comboboxes, spinners, listboxes,
  and date pickers, which the settings design already avoids for this reason.

**What genuinely branches:**

- **The dark title bar attribute is 20 on build 19041 and later, and 19 before that.** The documented value is 20 and
  the docs claim Windows 11, which is conservative: it works from Windows 10 20H1. Since our floor is 19045, we only
  ever need 20. **Check winit first, though.** winit 0.30's `platform_impl/windows/dark_mode.rs` does not use
  `DwmSetWindowAttribute` at all: it gates on build 17763 or later, calls `SetWindowTheme(hwnd, "DarkMode_Explorer")`,
  and sets the dark title bar through the undocumented `SetWindowCompositionAttribute` with `WCA_USEDARKMODECOLORS`.
  `Window::set_theme` may already cover us on 19045, in which case writing our own is duplicated work. Verify
  empirically before writing a line of it.
- **Windows 10 fails to repaint the title bar after an explicit light-to-dark switch.** wxWidgets forces it with a
  `WM_NCACTIVATE` toggle pair, gated to Windows 10 only. Copy that.
- **`DWMWA_WINDOW_CORNER_PREFERENCE` (33) for owner-drawn popups only**, gated on build 22000 or later. Not needed
  unless we own-draw menus, which we are not doing in v1.

**What does not exist for us at all:**

- **Mica and Acrylic.** `DWMWA_SYSTEMBACKDROP_TYPE` needs build 22621, and more importantly it is pointless here: a wgpu
  HWND swapchain paints the entire client area, so a DWM backdrop sits fully occluded behind it. Getting system material
  behind our own content would need `CreateSwapChainForComposition` plus DirectComposition, which wgpu's DX12 backend
  does not use for HWND surfaces. **There is no Windows equivalent of macOS's Liquid Glass in v1, and there should not
  be one.** Windows 10 has neither in any supported form regardless. One code path: opaque everywhere.
- **A custom caption.** `SettingKey::TitleBar` is already `NotApplicable` on Windows because a Win32 client area starts
  below the caption. Accept winit's standard decorations. A custom caption would need `WM_NCCALCSIZE` and it buys
  nothing.

### Dark mode, and the part of it we are choosing not to build

**There is still no supported public dark-mode API for Win32 common controls, as of 2026-08.** Microsoft acknowledges
the gap in a discussion that has run from 2020 to 2026 with no API and no timeline. What every real app does, and what
we will do:

- Dynamic-load `uxtheme.dll` and `GetProcAddress` **ordinal 135**, which is `SetPreferredAppMode(PreferredAppMode)` on
  build 18362 and later, and call it with `AllowDark`. (On 17763 the same ordinal was `AllowDarkModeForApp(BOOL)`, a
  different signature, but that build is below our floor so we only ever need the newer form.)
- Then `AllowDarkModeForWindow` (ordinal 133) plus `SetWindowTheme(hwnd, L"DarkMode_Explorer", nullptr)` per control,
  and `FlushMenuThemes` (ordinal 136).
- **Gate on build 18362 or later**, the way wxWidgets does. Do **not** copy `win32-darkmode`'s `CheckBuildNumber()`,
  which is an exact-match allowlist of `{17763, 18362, 18363, 19041}` and therefore **refuses to enable dark mode on
  19045, which is our actual floor.** That would be a silent, hard-to-spot bug on the exact configuration we committed
  to supporting.
- Read the user's preference from `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\ AppsUseLightTheme`
  (DWORD, 1 means light). That is what PowerToys, WPF, and WinForms read. `ShouldAppsUseDarkMode` (ordinal 132), which
  winit uses, has reports of returning `true` unconditionally on Windows 11 23H2.
- React to changes on `WM_SETTINGCHANGE` where `lParam` is `"ImmersiveColorSet"`.
- **Always suppress dark mode under `SPI_GETHIGHCONTRAST`.** High contrast is an accessibility setting and it wins.

**What we deliberately do not build in v1: owner-drawn dark menus.** muda gives us a `MenuTheme` for the menu bar, but
its docs are explicit that "the theme only affects the menu bar itself and not submenus or context menu". So in a dark
theme, the bar is dark and the dropdowns are light. Fixing that means owner-drawing every menu item, which also loses
the automatic Windows 11 rounding on the menu window (wxWidgets re-applies it by hand on `WM_MENUBAR_INITMENU`,
Windows-11-only, skipped under high contrast). That is a meaningful amount of code and two new branches for a cosmetic
gap that Notepad++ and many other well-liked Windows apps have lived with for years. Revisit if it grates.

Two smaller honest limits in the same family: scrollbars in dark mode need an IAT hook on `OpenNcThemeData` to look
right, and no single theme name covers all control types (`DarkMode_Explorer`, `DarkMode_ItemsView::ListView`,
`DarkMode_CFD` for combos). Reports put `DarkMode_Explorer` at "about 95% of the way" with poor highlight contrast on
Windows 11. Take the 95% and move on.

## What we deliberately don't build

Principle 3 is elegant simplicity, and 79 missing entries is a lot of surface. This section is the part of it that
should not exist on Windows.

**`NotApplicable` candidates, with the reason string each would carry:**

- **`MenuItemKey::CloseWindow`**: "Prvw has one window on Windows, and a Windows app with no windows is an invisible
  process rather than a running app. Closing that window is exiting, which File → Exit already does."

That is the only new one among the 79. Everything else in them is genuinely reachable on Windows under this design,
which is worth stating plainly: the design does not use `NotApplicable` as a way to make the number go down. The
registry's own docs warn that using the escape hatch without a real platform fact is how the whole layer rots, and one
honest entry is the right count here. The browse-mode status bar carries a second `NotApplicable`, but it faces the
other way (macOS, not Windows) and it is spelled out in its own section below.

**Things we build differently rather than skip**, restated so nobody mistakes them for gaps: `SettingKey::TitleBar` and
`CommandKey::TitleBar` are already `NotApplicable` and stay that way; `SettingKey::FileAssociations` becomes a
registration button plus a deep link and stays `Present`; About moves to Help and Settings moves to Tools, which the
registry handles as a coverage-arm comment because `Menu` is the product's shared grouping.

**Things we simply don't build, which are not registry entries and therefore need no reason string:**

- **Any Mica, Acrylic, or Liquid Glass equivalent.** Occluded by the swapchain, as above.
- **A custom caption or title strip.** Nothing covers the image on Windows.
- **Owner-drawn dark menus** in v1.
- **A menu bar auto-hide setting**, with or without Alt-reveal. Reasoned out under "Menu bar and fullscreen".
- **A task dialog for About**, or a second dialog mechanism of any kind. Reasoned out under "The about box".
- **The Windows 11 SettingsCard look.** No inbox control, no open-source Win32 precedent, and Microsoft's own answer is
  "use XAML Islands".
- **An onboarding window.** Replaced by the empty state.
- **A toolbar.** Windows viewers often have one; principle 2 says minimal chrome and the menu bar plus the keyboard
  covers everything. ACDSee had a toolbar and this is the one place we are deliberately not faithful to it.
- **A thumbnail size slider**, matching the macOS deferral.
- **An address bar in browse mode.**
- **Network location enumeration in the tree.**
- **MSIX packaging.** M7 already recommends Inno Setup or WiX for v1, because MSIX's sandbox complicates both the
  auto-updater and the QA server's localhost port.

## What this changes in the milestone estimates

The cross-platform plan's numbers were written before this design existed. Three of them move:

- **M4 (settings), estimated four weeks.** Unchanged. Six pages, 39 controls, a layout helper, a scrolling RAW page,
  dark mode, and DPI. Four weeks still looks right.
- **M5 (browse mode), estimated three to six weeks.** Unchanged, with the flip-model spike moved to the start rather
  than discovered in the middle. The `SetWindowTheme(L"Explorer")` line and the async volume-label read are the two
  details most likely to be missed.
- **M6 (onboarding and about), estimated one to two weeks. Now three or four days**, because the onboarding window does
  not exist and the about box is a small dialog on top of machinery M4 already built. The saved work moves into M1 step
  1's empty state, which was always in scope. M6 now depends on M4 rather than floating.

Also worth naming: **M1 step 9 (accelerators) grows slightly**, because the combined `with_msg_hook` closure now has to
serve both `TranslateAcceleratorW` and the modeless-dialog `IsDialogMessageW` path, and because we found no prior art
for the second half. Building the hook with both branches from the start, even before any dialog exists, avoids
reworking it in M4.

And one new item that is not a milestone: **"Show in File Explorer" and "Reveal in Finder" are a change of their own,
landing after M1 on both platforms at once.** Call it a day or two for the pair. It is small enough that folding it into
a milestone would bury it, and cross-platform enough that giving it to one milestone would make the platforms diverge
for no reason.

## Decisions that span sections

Five choices the sections above lean on. Three are argued where they apply and are only named here; two need their own
detail and get it.

1. **Slideshow is bare `S` on both platforms, and Ctrl+S stays bound to nothing.** See "Accelerators, and where they
   differ from macOS".
2. **The menu bar is always visible in windowed mode and gone in fullscreen, with no auto-hide and no setting in v1.**
   See "Menu bar and fullscreen".
3. **The browse-mode status bar gets built, Windows-only.** Below.
4. **"Show in File Explorer" and "Reveal in Finder" both get built, on both platforms, as their own change after M1.**
   Below.
5. **The about box is a small dialog of ours, not a `TaskDialogIndirect`.** See "The about box".

### The browse-mode status bar

**Coverage: Windows `Present`, macOS `NotApplicable`, Linux `Missing`.** The macOS reason string:

> A status bar pinned to the window's bottom edge is an Explorer idiom, and Finder's own is off by default. Prvw's macOS
> browse mode says what it has to in place instead: each grid cell wears its filename, an empty folder shows "(No
> images)", and an overdue scan shows "Loading…" over the tree pane.

Linux stays `Missing` rather than `NotApplicable` because the Linux desktop does not answer with one voice: a status bar
is canonical in KDE's Dolphin and absent from modern GNOME's Files. That is a Linux spec's call to make, and `Missing`
is what an undecided entry honestly looks like.

**Where the entry goes is M5's first question, because layer 1 has no registry that fits.** `SettingKey`, `MenuItemKey`,
and `CommandKey` model settings, menu items, and actions; a passive readout is none of the three, and `parity/CLAUDE.md`
already names this as the hole ("There's no entry for 'the menu bar' or 'the settings window' itself"). M5 either grows
layer 1 a kind for readouts or accepts that this is the case that finally forces one. Don't wedge it into `CommandKey`
just to get a row in the table.

**What macOS is missing, said plainly.** The reason string above is true about the _surface_: a bottom status bar is not
a Mac idiom, and the filename readout already has a Mac home in the cell labels. It is not the whole story about the
_content_. Two of the three things the Windows status bar shows have no macOS browse-mode home at all:

- **The folder's image count.** macOS shows "3 / 40" in the title-bar strip in image mode (`App::titlebar_text`), and
  browse mode deliberately hides those labels: `browser::State::sync_native` calls
  `window::set_titlebar_labels_hidden(window, true)`, because browse stops redrawing and the stale image title would
  otherwise linger over the native UI. So while browsing on a Mac, nothing says how many images the folder holds.
- **The selected image's dimensions.** They live in the Exif overlay's "Size" row, which is an image-mode overlay bound
  to `E`. Browse mode never shows it.

That is a macOS gap, not a Windows feature, and a `NotApplicable` on the surface must not be read as covering it. It
deserves the same treatment as the reveal pair below: a small change of its own that gives the Mac browser somewhere to
say those two things in a Mac idiom, most likely the title-bar strip it already owns, repurposed for browse mode. Not
scheduled here; named so it does not disappear behind a reason string.

### Show in File Explorer and Reveal in Finder

**Build both, on both platforms, as their own change after M1.** Three reasons:

- **It is a gap on both platforms rather than a Windows feature.** macOS has no "Reveal in Finder" today either, and
  framing it as a Windows item would hide that.
- **It is small on each.** `ShellExecuteW` against `explorer.exe` with `/select,<path>` on Windows; `NSWorkspace`'s
  `activateFileViewerSelectingURLs:` on macOS.
- **Doing both at once means parity never diverges.** The new `MenuItemKey` and `CommandKey` land with both arms
  `Present` in the same commit, so the table never carries a row that is `Missing` on one side while somebody decides.

It belongs in the File menu and in the image context menu. In browse mode it also belongs in the grid's right-click
menu, which is M5's to wire once the command exists.

## What we could not verify

There is no Windows machine available, so nothing in this document has been run. Beyond that, these specific claims are
reasoned or triangulated rather than measured, and each is somewhere a surprise could hide:

- The on-disk size of a self-contained unpackaged WinUI 3 app in 2026. The 40 to 90 MB figure comes from a 2022
  measurement, a 2024 report, and Microsoft's runtime file list.
- Whether `with_msg_hook` plus `IsDialogMessageW` composes cleanly. No existing Rust project does this.
- Whether attaching a menu bar with `SetMenu` confuses winit's client-area or DPI math.
- Whether wgpu keeps rendering during a system-owned nested modal loop.
- Whether flip-model swapchain presentation over child HWNDs behaves as reasoned. The design is built so this does not
  matter, and there is a named fallback, but it is unverified.
- Whether winit's existing dark-title-bar handling already covers build 19045, which would make our own implementation
  redundant.
- Whether `DwmSetWindowAttribute(33, ...)` fails cleanly on Windows 10. No Microsoft doc states it; version-check rather
  than relying on graceful failure.
- Whether Segoe UI Variable ever shipped to Windows 10.
- The exact semantics of the `DarkMode_DarkTheme` window theme added in Windows 11 25H2 (build 26200).
- The precise list of comctl32 controls Windows 11 restyled. The best source is a community-filed, Microsoft-hosted
  issue that was closed with no Microsoft response.
- The compile-time and binary-size delta between `windows` and `windows-sys` for our feature set. No published 2026
  benchmark exists; measure with `cargo build --timings` if it matters.
- Whether `explorer.exe /select,<path>` and `NSWorkspace`'s `activateFileViewerSelectingURLs:` behave as the reveal
  decision describes. Both are long-standing and widely used, and neither was run for this document.
- That Finder's own status bar is off by default, which the status bar's macOS reason string leans on. Everything else
  in that string comes from reading `browser/` and `app.rs`.
