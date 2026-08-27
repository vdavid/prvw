# Platform: Windows

Windows-specific glue. Mirrors `platform/macos/`: `windows.rs` beside this directory holds the small queries that don't
warrant a file (console attach, installed RAM, the wheel and double-click preferences, the system UI font name, the
monitor work area), and anything with real substance gets its own module here.

| File                | Purpose                                                                                                          |
| ------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `clipboard.rs`      | Copy the current image to the clipboard as the original file plus sRGB pixels (Edit → Copy, Ctrl+C, right-click) |
| `dark_mode.rs`      | Dark chrome for our Win32 windows: the `uxtheme` ordinals, and the pure decision about when to use them          |
| `msg_hook.rs`       | The one hook in winit's message pump: display changes, menu accelerators, and the seam a modeless dialog uses    |
| `print.rs`          | File → Print: `PrintDlgW` on a worker thread, then the image drawn onto one page with GDI                        |
| `ui_common.rs`      | The sRGB re-decode Copy and Print share                                                                          |
| `window_capture.rs` | Debug-only window photograph for the QA server's `screenshot_window` tool                                        |

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
threaded COM apartment and buys a page-range UI that a one-page image print has no use for.

## Decision: the page is `HORZRES` × `VERTRES`

**Why:** that's the **printable** area in device pixels. `PHYSICALWIDTH` / `PHYSICALHEIGHT` are the whole sheet
including the hardware margins, so laying the photo out against those puts its edges where the printer can't put ink.
The fit inside it is `crate::printing::aspect_fit`, shared with macOS and tested from any host; the enlargement it does
for a small image is deliberate, since "print this photo" means fill the paper.

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

`about::windows` is the first caller. M4's settings dialog is the next one, and it should use this module rather than
growing its own.

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

Everything in this directory type-checks and lints through `./scripts/check.sh --check windows-cross`, and **nothing
here has ever run**. The clipboard's paste targets, the right-click menu, the drop path, and every part of printing
(does the owner window disable properly from another thread? does the page come out the right way up?) all need a person
at a Windows box, and so does every line of `dark_mode.rs`: the ordinals are undocumented, so "it compiles" says nothing
about whether the box comes up dark. The capture in particular has no E2E coverage: `tests/e2e_macos.rs` exercises the
macOS path, and the equivalent Windows test needs a Windows box to be worth writing.
