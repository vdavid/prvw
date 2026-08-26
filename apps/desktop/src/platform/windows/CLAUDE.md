# Platform: Windows

Windows-specific glue. Mirrors `platform/macos/`: `windows.rs` beside this directory holds the small queries that don't
warrant a file (console attach, installed RAM, the wheel and double-click preferences, the system UI font name, the
monitor work area), and anything with real substance gets its own module here.

| File                | Purpose                                                                                                          |
| ------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `clipboard.rs`      | Copy the current image to the clipboard as the original file plus sRGB pixels (Edit → Copy, Ctrl+C, right-click) |
| `msg_hook.rs`       | The one hook in winit's message pump: menu accelerators, and the seam a modeless dialog registers through        |
| `window_capture.rs` | Debug-only window photograph for the QA server's `screenshot_window` tool                                        |

Everything here is behind `#[cfg(target_os = "windows")]` at the `platform` import site, so a Mac never compiles it.
`./scripts/check.sh --check windows-cross` is what gives it a feedback loop.

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

**Decision:** `CF_HDROP` carries the path only when `clipboard::shell_path` can produce a plain Win32 spelling of it.
Otherwise the clipboard gets the pixels and no file list.

**Why:** `src/paths.rs` keeps the `\\?\` prefix on every path the app carries, because that prefix is what lifts
`MAX_PATH`, and it names the shell as the one boundary where the prefix has to come off. Taking it off puts the path
back under every limit it was lifting, so a path past 260 characters, a component ending in a dot or a space, or a DOS
device name (`NUL.jpg` is the null device) would be handed to Explorer and quietly resolve to something else. That
module says the first shell call site owns this rule; `CF_HDROP` is it. Fold the helper into `paths.rs` when it next
gets touched — it belongs there, and there was no shell caller when it was written.

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
here has ever run**. The clipboard's paste targets, the right-click menu, and the drop path all need a person at a
Windows box. The capture in particular has no E2E coverage: `tests/e2e_macos.rs` exercises the macOS path, and the
equivalent Windows test needs a Windows box to be worth writing.
