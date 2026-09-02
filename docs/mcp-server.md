# MCP server reference

Prvw embeds an HTTP/MCP server for agent integration and E2E testing. Port controlled by `PRVW_QA_PORT` env var (default
19447, set to 0 to disable).

## MCP endpoint

`POST /mcp`: JSON-RPC 2.0 over HTTP (streamable HTTP transport).

All MCP tool calls wait synchronously for the event loop to process the command before returning. The response includes
both a text confirmation and a `state` field with the current app state as JSON.

## Tools

### Navigation

- **navigate**: Navigate to next/prev image. Params: `direction` ("next" or "prev"; also accepts "forward"/"backward" as
  aliases).
- **open**: Open a file by path. Params: `path` (absolute).
- **loop_navigation**: Toggle loop navigation. When on, Next at the last image wraps to the first and Previous at the
  first wraps to the last. Default off. No params.
- **key** with `key: "Home"` or `key: "End"`: Absolute jump to the first or last image. No-op when already at the
  target.

### View

- **zoom**: Set absolute zoom level. Params: `level` (float, 1.0 = fit, or "fit"/"actual").
- **zoom_in**: Zoom in one step (25%).
- **zoom_out**: Zoom out one step (25%).
- **scroll_zoom**: Scroll-wheel zoom at cursor position. Params: `delta` (float), `cursor_x`, `cursor_y` (pixels).
- **fullscreen**: Control fullscreen. Params: `mode` ("on", "off", "toggle").
- **refresh**: Re-display the current image, re-applying zoom and settings.
- **histogram**: Toggle the histogram overlay. No params.
- **exif_info**: Toggle the EXIF info overlay. Renders only when the current image carries EXIF data. No params.
- **set_cursor_position**: Move the cached cursor (drives histogram hover from tests). Params: `x`, `y` (logical
  pixels).

### Window

- **set_window_geometry**: Set window position/size. Params (all optional): `x`, `y`, `width`, `height`.

### Settings

- **auto_fit_window**: Enable/disable auto-fit. Params: `enabled` (bool).
- **enlarge_small_images**: Enable/disable small image enlargement. Params: `enabled` (bool).

### Utility

- **key**: Simulate a key press. Params: `key` (web convention name).
- **screenshot**: Capture current view as base64 PNG. Renders through the offscreen wgpu path, so the result has no
  overlays, no title-bar strip, and no vibrancy.
- **screenshot_window** (debug builds only, macOS and Windows): Capture the entire native window as a person sees it,
  including histogram and EXIF overlays, title bar, window chrome, and any modal panels. Same base64 PNG contract on
  both platforms, so nothing above it branches. macOS shells out to `/usr/sbin/screencapture -l <windowNumber>` and
  needs Screen Recording permission: the system prompts on first use, and the first call may come back black until you
  grant it. Windows asks the window to draw itself with `PrintWindow`, which needs no permission and works on a window
  that's unfocused or occluded. Not registered in release builds, and not on Linux, which has no capture path yet.

## Resources

- **prvw://state**: Current app state as JSON (file, zoom, pan, fullscreen, window/image geometry, settings, title,
  `loop_navigation`, `slideshow_running`, `cache_indices` (sorted directory indices currently in the image cache), the
  folder-scan fields `scan_pending`, `queued_nav`, and `read_progress`, and the browse-mode fields `view_mode`,
  `focused_pane`, `browse_selected_folder`, `browse_grid_selected`, `browse_grid_count`, `browse_reveal_pending`).

  `queued_nav` is the move made while the folder was still being scanned, applied when the scan lands:
  `{"anchor": "current" | "first" | "last", "delta": <signed steps>}`, or `null` when nothing is queued (which is what a
  left-then-right pair nets back to).

  `read_progress` is how full the read progress bar under the "Loading…" overlay is, `0.0` to `1.0`, or `null` when no
  bar is drawn. Null is the normal case: a file that reads inside the overlay's 150 ms delay never shows one.

- **prvw://settings**: Current settings from disk as JSON (auto_update, auto_fit_window, enlarge_small_images,
  loop_navigation).
- **prvw://menu**: Menu bar structure.
- **prvw://diagnostics**: Cache state, navigation timing, memory usage.

## Simple HTTP endpoints

All endpoints also available as simple HTTP for cURL debugging. POST endpoints return the updated app state as JSON
(`application/json`) after the command completes. GET `/state` also returns JSON.

| Method | Path                  | Body                                                      | Response         |
| ------ | --------------------- | --------------------------------------------------------- | ---------------- |
| GET    | /state                | -                                                         | State JSON       |
| GET    | /settings             | -                                                         | Settings JSON    |
| GET    | /menu                 | -                                                         | Menu text        |
| GET    | /parity               | -                                                         | Parity JSON      |
| GET    | /screenshot           | -                                                         | PNG bytes        |
| GET    | /diagnostics          | -                                                         | Diagnostics text |
| POST   | /key                  | key name                                                  | State JSON       |
| POST   | /navigate             | "next", "prev", "forward", or "backward"                  | State JSON       |
| POST   | /zoom                 | "fit", "actual", or float                                 | State JSON       |
| POST   | /zoom-in              | -                                                         | State JSON       |
| POST   | /zoom-out             | -                                                         | State JSON       |
| POST   | /scroll-zoom          | JSON: `{"delta": 1.0, "cursor_x": 400, "cursor_y": 300}`  | State JSON       |
| POST   | /fullscreen           | "on", "off", "toggle"                                     | State JSON       |
| POST   | /open                 | file path                                                 | State JSON       |
| POST   | /auto-fit             | "on" or "off"                                             | State JSON       |
| POST   | /enlarge-small        | "on" or "off"                                             | State JSON       |
| POST   | /window-geometry      | JSON: `{"x": 100, "y": 100, "width": 800, "height": 600}` | State JSON       |
| GET    | /window-diagnostics   | -                                                         | Text (debug)     |
| POST   | /zoom-window          | -                                                         | State JSON       |
| POST   | /click-zoom-button    | -                                                         | State JSON       |
| POST   | /refresh              | -                                                         | State JSON       |
| POST   | /browse/select-folder | absolute folder path (test-only, macOS)                   | State JSON       |
| POST   | /browse/select-grid   | grid index (test-only, macOS)                             | State JSON       |
| POST   | /browse/open          | - (open the selected grid image; test-only, macOS)        | State JSON       |

The last three endpoints above are debug-build, macOS-only window-chrome hooks (they don't exist in release):

- `GET /window-diagnostics` dumps the window's AppKit view/layer tree — every titlebar view's frame in window
  coordinates, the standard window buttons, `styleMask`, `collectionBehavior`, and the corner-radius/mask geometry. It's
  the tool for "where does AppKit think the traffic lights are, and where do they draw?" and for checking the window's
  appearance state after a zoom or fullscreen round trip.
- `POST /zoom-window` performs the native `zoom:`; `POST /click-zoom-button` sends `performClick:` to the green traffic
  light. The two differ (the button's action, not `zoom:`, is what macOS 26 routes through fullscreen), so use the
  button one to exercise what a real click does.

The three `/browse/*` endpoints are test-only driving hooks: the QA path can't synthesize native outline/collection-view
clicks, so integration tests drive tree selection, grid selection, and open through them. macOS-only (browse mode is);
they return 400 off macOS.
