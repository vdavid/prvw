# Platform: Windows

Windows-specific glue. Mirrors `platform/macos/`: `windows.rs` holds the small queries that don't warrant a file
(console attach, installed RAM, the wheel and double-click preferences, the system UI font name), and anything with real
substance gets its own module here.

| File                | Purpose                                                                                 |
| ------------------- | --------------------------------------------------------------------------------------- |
| `msg_hook.rs`       | The one message hook in winit's pump: menu accelerators through `TranslateAcceleratorW` |
| `window_capture.rs` | Debug-only window photograph for the QA server's `screenshot_window` tool               |

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

## Gotchas

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

Everything here type-checks and lints through `./scripts/check.sh --check windows-cross` and nothing here has run. The
capture in particular has no E2E coverage: `tests/e2e_macos.rs` exercises the macOS path, and the equivalent Windows
test needs a Windows box to be worth writing.
