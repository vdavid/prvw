# Platform: Windows

Windows-specific glue. Mirrors `platform/macos/`: `windows.rs` beside this directory holds the small queries that don't
warrant a file (console attach, installed RAM, the wheel and double-click preferences, the system UI font name, the
monitor work area), and anything with real substance gets its own module here.

| File                | Purpose                                                                                                                                       |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `clipboard.rs`      | Copy the current image to the clipboard as the original file plus sRGB pixels (Edit → Copy, Ctrl+C, right-click)                              |
| `dark_mode.rs`      | Dark chrome for our Win32 windows: the `uxtheme` ordinals, and the Win32 half of `crate::chrome`'s colour policy                              |
| `msg_hook.rs`       | The one hook in winit's message pump: display changes, menu accelerators, and the seam the About box and the settings dialog register through |
| `print.rs`          | File → Print: `PrintDlgW` on a worker thread, then the image drawn onto one page with GDI                                                     |
| `ui_common.rs`      | The sRGB re-decode Copy and Print share                                                                                                       |
| `window_capture.rs` | Debug-only window photograph for the QA server's `screenshot_window` tool                                                                     |

Everything here is behind `#[cfg(target_os = "windows")]` at the `platform` import site, so a Mac never compiles it.
`./scripts/check.sh --check windows-cross` is what gives it a feedback loop.

## `WM_DISPLAYCHANGE` reaches the app through the hook, and only through the hook

winit doesn't handle the message, so nothing else in the app hears a monitor arriving or leaving, a resolution change,
or an ICC profile re-associated with a display in place. Any of those changes which profile the image on screen should
be transformed into, so `msg_hook` posts `AppCommand::DisplayChanged` and falls through without consuming the message.

That stage runs before the dialog and accelerator stages because it is the only one that just watches. See
`color::display_profile` for what the app does with it, and the module docs in `msg_hook.rs` for the ordering rule.

## Copy loads the file, not the buffer on screen

`clipboard.rs` re-decodes the original file rather than reusing `App`'s decoded pixels, for the same reason macOS does
(`platform/macos/CLAUDE.md`): that buffer is already transformed to the display profile and may be half-float HDR, so
handing it over would shift colours in whatever pastes it. The target profile is sRGB, because a DIB carries no ICC
profile and a consumer will read it as sRGB whatever it actually is.

One difference from macOS, in Windows' favour: a RAW file is developed by Prvw's own pipeline here, so a copied RAW
matches the viewer. macOS goes through ImageIO and doesn't.

## Decision: the copy runs on a worker thread

**Decision:** `copy_image_file` spawns a thread and returns; the clipboard write happens there.

**Why:** the decode is a full decode, seconds for a large RAW, and the winit thread can't afford it (principle 4: never
block the main thread). Win32 allows it — `OpenClipboard` binds to the calling thread, and none of it needs a window.
The cost is that failures are a log line rather than a return value, which is why every failure path in there logs.

## Decision: the print dialog runs on a worker thread too

**Decision:** `print_image_file` spawns `prvw-print`, and `PrintDlgW`, the decode, and the spooling all happen there.

**Why:** `PrintDlgW` is modal — it opens a message loop and doesn't return until the person picks a printer. On winit's
thread that's the starved pump this project keeps warning about: `about_to_wait` stops, the slideshow timer freezes,
`EventLoopProxy` events stall. Win32 makes the worker an easy answer, because a common dialog is modal to its **own**
thread: naming the main window as `hwndOwner` still disables it and keeps the dialog in front, so it looks app-modal to
the person while winit keeps pumping. `open_dialog.rs` already does the same for the file picker.

`PrintDlgW` and not `PrintDlgExW`: the newer property-sheet version requires the calling thread to be an apartment-
threaded COM apartment and buys a page-range UI that a one-page image print has no use for. On Windows 11 the choice
makes no difference to what the person sees, because the unified dialog replaces both (see below).

## Decision: the page is `HORZRES` × `VERTRES`

**Why:** that's the **printable** area in device pixels. `PHYSICALWIDTH` / `PHYSICALHEIGHT` are the whole sheet
including the hardware margins, so laying the photo out against those puts its edges where the printer can't put ink.
The fit inside it is `crate::printing::fit_to_page`, shared with macOS and tested from any host; the enlargement it does
for a small image is deliberate, since "print this photo" means fill the paper.

## Decision: the auto-rotate turns the pixels, because GDI won't turn a blit

**Decision:** when `printing::fit_to_page` says the photo prints bigger turned, `draw_one_page` transposes the decoded
buffer with `printing::rotate_quarter_turn_clockwise`, swaps `width` and `height`, and blits that. macOS turns the
page's coordinate system instead.

**Why:** `StretchDIBits` scales and does nothing else, and the alternative — `SetGraphicsMode(GM_ADVANCED)` plus
`SetWorldTransform` — is honoured on a printer DC entirely at the driver's discretion, so a rotation could silently not
happen on somebody's printer. A transpose always happens. It costs one extra pass over a buffer that's about to be
copied to the spooler anyway, on the `prvw-print` worker, and it stays top-down so the negative `biHeight` still holds.

The turn is decided on the dimensions `decode_srgb` returns, which are post-EXIF-orientation: `decoding::load_image`
rotates the buffer and reports the rotated size. Deciding on the file's stored size would print upright photos sideways.

Clockwise, matching EXIF orientation 6, so the app only ever turns a photo one way.

## Gotcha: Windows 11 says we don't support print preview, and it says that to every GDI app

**Gotcha:** on Windows 11 22H2 and later, the dialog that opens is the unified print dialog rather than the common
dialog we asked for. It replaces `PrintDlg` and `PrintDlgEx` for every classic app, it carries a preview pane, and for
us that pane reads "This app doesn't support print preview". Nothing is wrong with our call and the print goes through:
Notepad and WordPad show the same message.

**Why the pane is empty:** the preview is app-supplied, and only the WinRT pipeline can supply it. `PrintManager` hands
the dialog an `IPrintDocumentSource`, and the dialog calls back into `IPrintPreviewPageCollection::Paginate` and
`MakePage` for each page as it needs one. A GDI caller has no part in that protocol: `PD_RETURNDC` asks for a device
context, and the drawing happens after the dialog closes, so at preview time no page exists to show. There is no
documented way to fill that pane from a `PrintDlgW` caller, and Microsoft's own answer to people who ask is a registry
key that brings the old dialog back.

**What supplying a real preview would cost:** the whole print path moves to WinRT. `PrintManagerInterop::GetForWindow`
binds to the main window and therefore to winit's thread, which would need a `DispatcherQueue` of its own to receive the
callbacks; the content would have to be produced twice, as DXGI surfaces for `IPrintPreviewDxgiPackageTarget` and as an
XPS package for the job itself, so a Direct2D and XPS pipeline would grow beside the wgpu one. That is one of the
largest subsystems in the Windows port, none of it checkable from a Mac, and it buys a thumbnail of the photo already
filling the screen. ❌ Don't.

**Confirming it's Windows and not us:**
`reg add "HKCU\Software\Microsoft\Print\UnifiedPrintDialog" /v PreferLegacyPrintDialog /t REG_DWORD /d 1 /f` brings the
old common dialog back for the current user, and it has no preview pane and so no message; `reg delete` on the same
value restores the modern one. An app can force the same per call by passing `PD_ENABLEPRINTHOOK` with a hook procedure
that does nothing, and ❌ we deliberately don't: the unified dialog is what Windows 11 puts in front of people
everywhere else, and trading it for a Windows 2000-era dialog to hide a sentence Microsoft wrote costs more than the
sentence does.

**The consequence that does bite:** the unified dialog drops settings the app pre-loads into `hDevMode`, orientation
being the reported one. So "print this landscape photo on landscape paper" can't be done by seeding the DEVMODE before
the dialog. It has to rotate the image onto whatever page the DC comes back describing, which is what the decision below
does.

## Decision: the page is drawn with the `HALFTONE` stretch mode

**Why:** GDI's default is `BLACKONWHITE`, which ANDs the colour values of the scan lines it eliminates. Every photo
print here shrinks the image (a 24 MP photo is about 6,000 px wide, where an A4 sheet at 300 dpi holds about 2,500), so
the default would darken and alias the paper. `HALFTONE` averages those pixels instead, and it wants `SetBrushOrgEx`
called right after it or its brush misaligns. Both calls are best-effort, since a driver that scales the DIB its own way
ignores them.

## Gotcha: a printer has no alpha, and a dropped alpha byte prints black

**Gotcha:** the decoder hands back straight-alpha RGBA8, and GDI's 32-bit `BI_RGB` layout is B, G, R, and a byte it
ignores. Reordering alone would print a transparent PNG's background as black, so `printing::flatten_onto_white_bgra`
composites each channel onto white first — which is what macOS gets for free from `drawInRect:` compositing `SourceOver`
onto the page.

## Gotcha: the dialog's handles are ours even when the person cancels

**Gotcha:** `PrintDlgW` allocates `hDevMode` and `hDevNames` whether or not it returns true, and both are the caller's
to `GlobalFree`. And a false return is not necessarily a failure: `CommDlgExtendedError()` answering zero means the
person cancelled, which deserves a debug line rather than a warning.

## Decision: three formats, and `CF_DIB` is written by hand

**Decision:** `CF_HDROP` + `CF_DIB` (24-bit, alpha composited over white) + `CF_DIBV5` only when the image really has
transparency, plus "Preferred DropEffect" saying copy.

**Why:** Windows synthesises `CF_DIB` from `CF_DIBV5` by handing over the same 32-bit pixels, and most consumers read
that fourth byte as padding rather than alpha, so a transparent PNG pastes as garbage. Writing `CF_DIB` explicitly both
suppresses the synthesis and removes the byte to misread. `crate::clipboard` holds the layouts and the full reasoning,
and is where the tests are: they're pure functions on purpose, so a Mac can check what a Windows user will paste.

## Decision: the window capture is `PrintWindow`, not a screen blit and not a wgpu readback

**Why:** three candidates, and only one answers the question the tool exists for.

- A `BitBlt` off the screen device context copies whatever is on those coordinates, so an occluded window comes back
  with the window on top of it. The E2E harness deliberately opens its windows unfocused and behind everything
  (`window::background_window_requested`), so this would be wrong exactly where it's used most.
- A wgpu surface readback would be portable and would pick up the overlays, but the surface is the client area only: it
  can't see the menu bar or the window frame, and a swapchain texture isn't created with `COPY_SRC`, so it would also
  mean reconfiguring the surface and restructuring the render path for a debug-only feature. It would be a third kind of
  screenshot, not a replacement for either of the two we have.
- `PrintWindow` asks the window to draw itself, so an occluded, unfocused, or partly off-screen window still comes back
  whole, and it costs the renderer nothing on the hot path.

## Decision: a path with no legal shell form loses the file, not the copy

**Decision:** `CF_HDROP` carries the path only when `paths::shell_path` can produce a plain Win32 spelling of it.
Otherwise the clipboard gets the pixels and no file list.

**Why:** `src/paths.rs` keeps the `\\?\` prefix on every path the app carries, because that prefix is what lifts
`MAX_PATH`, and it names the shell as the one boundary where the prefix has to come off. Taking it off puts the path
back under every limit it was lifting, so a path past 260 characters, a component ending in a dot or a space, or a DOS
device name (`NUL.jpg` is the null device) would be handed to Explorer and quietly resolve to something else.
`shell_path` lives in `paths.rs` with the rest of the rule and is tested there; `CF_HDROP` is its first caller.

## Decision: dark mode is three undocumented ordinals, gated on a build number

**Decision:** `dark_mode.rs` dynamic-loads `uxtheme.dll` and calls ordinals 135 (`SetPreferredAppMode`) and 133
(`AllowDarkModeForWindow`), then `SetWindowTheme(hwnd, "DarkMode_Explorer", null)` per control. Every call is
best-effort: a missing export leaves the window light rather than half-painted.

**Why:** there is still no supported public dark-mode API for Win32 common controls, and this is what every app that
does it reaches for. `docs/specs/windows-ui-design.md` collects the sources. The build gate is at 18362, where ordinal
135 took its current signature. ❌ Don't copy `win32-darkmode`'s `CheckBuildNumber`, an exact-match allowlist that
refuses 19045 — Prvw's actual support floor.

`theme_for` is the whole decision and it's pure, so it's asserted rather than eyeballed: high contrast wins outright
(it's an accessibility setting), then the build gate, then `AppsUseLightTheme` from the registry. That last one is what
PowerToys, WPF, and WinForms read; `ShouldAppsUseDarkMode` (ordinal 132) has reports of answering `true` unconditionally
on Windows 11 23H2.

It lives in `crate::chrome` rather than here, along with the colour table, because neither needs Win32 and this file
can't be run from a Mac. `dark_mode` is the half that reads the three inputs out of the system, turns a `chrome::Color`
into a `COLORREF`, keeps the brushes, and answers `WM_CTLCOLOR*`.

## Decision: one `WM_CTLCOLOR*` reply, keyed on the control's class

**Decision:** `dark_mode::paint_control` answers every `WM_CTLCOLOR*` the same way. It reads the control's window class
and asks `chrome::surface_for_class` which of the two surfaces it sits on, and the caller supplies only the ink (body,
or the dimmer secondary). The About box and the settings dialog both go through it.

**Why:** the message looks like it names the control class and doesn't. **A read-only or disabled edit sends
`WM_CTLCOLORSTATIC`**, so a handler that switches on the message paints a text field with the window's own colour. That
shipped, and it's what David saw the first time Settings ran on Windows: a grey slab where the file-extension list
should have had a field's background. Keying on the class makes the message irrelevant and the mistake unrepeatable.

## The first dialog through the hook is the About box

`msg_hook::register_dialog` was built in M1 for a caller that didn't exist yet. `about::windows` is it: a modeless popup
that registers on open and unregisters on `WM_DESTROY`, so `IsDialogMessageW` runs for its messages before accelerator
translation. That ordering is why typing inside a dialog can't fire a menu accelerator.

## Gotcha: `GlobalUnlock` reports failure on success

**Gotcha:** `GlobalUnlock` returns false with `GetLastError() == NO_ERROR` when the lock count reaches zero, which is
the normal outcome. Treating the `Result` as an error would log a warning on every successful copy, so the call site
discards it.

## Gotcha: who owns an `HGLOBAL` flips mid-call

**Gotcha:** `SetClipboardData` takes ownership of the block **only if it succeeds**. Freeing one afterwards corrupts the
clipboard; not freeing one it rejected leaks for the life of the process. `GlobalBlock` is the seam that makes this hard
to get wrong: it frees on drop, and handing it over consumes it.

## Gotcha: a popup menu runs a message loop, and that one is allowed

**Gotcha:** `AppMenu::show_image_context_menu` reaches `TrackPopupMenu`, which runs a modal message loop. The rule the
project keeps repeating ("never open a nested message loop") is about loops **we** open: a popup menu, like a menu-bar
drop-down or a title-bar drag, is system-owned and unavoidable, and winit's pump resumes when it closes. The chosen item
arrives as a `MenuEvent` on the next `about_to_wait`, the same route a menu-bar click takes.

## Gotchas: the window capture

- **`PW_RENDERFULLCONTENT` is not optional.** Prvw's client area is a GPU swapchain. Without that flag, `PrintWindow`
  captures through the legacy redirection path and a DirectComposition-backed surface comes back black.
- **GDI never writes the alpha byte.** A 32-bit DIB it blitted into is fully transparent as far as PNG is concerned, so
  `qa::window_capture::bgra_frame_to_png` forces every pixel opaque. Skip that and the capture decodes as an empty
  image, which looks like a broken renderer rather than a broken encoder.
- **Read the bits after `GdiFlush`.** GDI batches its drawing calls, and reading the DIB's memory goes around the batch.
- **Delete the device context before the bitmap.** GDI refuses to delete a bitmap that's still selected into a live DC,
  which is why `window_capture` declares its bitmap guard first and its DC guard second.
- **`GetWindowRect` includes the invisible resize border** DWM adds around a resizable window, so the capture may carry
  a few opaque-black pixels down each edge. Worth confirming on real hardware; cropping to `DWMWA_EXTENDED_FRAME_BOUNDS`
  is the fix if it's ugly.

## What hasn't met a Windows machine yet

Most of this directory type-checks and lints through `./scripts/check.sh --check windows-cross` and has never run.
Printing is the exception: the dialog opens from the worker thread on a real Windows 11 box, the app keeps responding
while it is up, and the flow completes. What the paper looks like is still unverified. So are the clipboard's paste
targets, the right-click menu, the drop path, and every line of `dark_mode.rs`: the ordinals are undocumented, so "it
compiles" says nothing about whether the box comes up dark. The capture in particular has no E2E coverage:
`tests/e2e_macos.rs` exercises the macOS path, and the equivalent Windows test needs a Windows box to be worth writing.
