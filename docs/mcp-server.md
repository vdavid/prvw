# MCP server reference

Prvw embeds an HTTP/MCP server for agent integration and E2E testing. Port controlled by `PRVW_QA_PORT` env var
(default 19447, set to 0 to disable).

## MCP endpoint

`POST /mcp` — JSON-RPC 2.0 over HTTP (streamable HTTP transport).

All MCP tool calls wait synchronously for the event loop to process the command before returning. The response includes
both a text confirmation and a `state` field with the current app state as JSON.

## Tools

### Navigation
- **navigate** — Navigate to next/prev image. Params: `direction` ("next" or "prev"; also accepts "forward"/"backward"
  as aliases).
- **open** — Open a file by path. Params: `path` (absolute).

### View
- **zoom** — Set absolute zoom level. Params: `level` (float, 1.0 = fit, or "fit"/"actual").
- **zoom_in** — Zoom in one step (25%).
- **zoom_out** — Zoom out one step (25%).
- **scroll_zoom** — Scroll-wheel zoom at cursor position. Params: `delta` (float), `cursor_x`, `cursor_y` (pixels).
- **fullscreen** — Control fullscreen. Params: `mode` ("on", "off", "toggle").
- **refresh** — Re-display the current image, re-applying zoom and settings.

### Window
- **set_window_geometry** — Set window position/size. Params (all optional): `x`, `y`, `width`, `height`.

### Settings
- **auto_fit_window** — Enable/disable auto-fit. Params: `enabled` (bool).
- **enlarge_small_images** — Enable/disable small image enlargement. Params: `enabled` (bool).

### Utility
- **key** — Simulate a key press. Params: `key` (web convention name).
- **screenshot** — Capture current view as base64 PNG.

## Resources

- **prvw://state** — Current app state as JSON (file, zoom, pan, fullscreen, window/image geometry, settings, title).
- **prvw://settings** — Current settings from disk as JSON (auto_update, auto_fit_window, enlarge_small_images).
- **prvw://menu** — Menu bar structure.
- **prvw://diagnostics** — Cache state, navigation timing, memory usage.

## Simple HTTP endpoints

All endpoints also available as simple HTTP for cURL debugging. POST endpoints return the updated app state as JSON
(`application/json`) after the command completes. GET `/state` also returns JSON.

| Method | Path | Body | Response |
|--------|------|------|----------|
| GET | /state | — | State JSON |
| GET | /settings | — | Settings JSON |
| GET | /menu | — | Menu text |
| GET | /screenshot | — | PNG bytes |
| GET | /diagnostics | — | Diagnostics text |
| POST | /key | key name | State JSON |
| POST | /navigate | "next", "prev", "forward", or "backward" | State JSON |
| POST | /zoom | "fit", "actual", or float | State JSON |
| POST | /zoom-in | — | State JSON |
| POST | /zoom-out | — | State JSON |
| POST | /scroll-zoom | JSON: `{"delta": 1.0, "cursor_x": 400, "cursor_y": 300}` | State JSON |
| POST | /fullscreen | "on", "off", "toggle" | State JSON |
| POST | /open | file path | State JSON |
| POST | /auto-fit | "on" or "off" | State JSON |
| POST | /enlarge-small | "on" or "off" | State JSON |
| POST | /window-geometry | JSON: `{"x": 100, "y": 100, "width": 800, "height": 600}` | State JSON |
| POST | /refresh | — | State JSON |
