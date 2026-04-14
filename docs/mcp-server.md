# MCP server reference

Prvw embeds an HTTP/MCP server for agent integration and E2E testing. Port controlled by `PRVW_QA_PORT` env var
(default 19447, set to 0 to disable).

## MCP endpoint

`POST /mcp` — JSON-RPC 2.0 over HTTP (streamable HTTP transport).

## Tools

### Navigation
- **navigate** — Navigate to next/prev image. Params: `direction` ("next" or "prev").
- **open** — Open a file by path. Params: `path` (absolute).

### View
- **zoom** — Set absolute zoom level. Params: `level` (float, 1.0 = fit).
- **zoom_in** — Zoom in one step (25%).
- **zoom_out** — Zoom out one step (25%).
- **scroll_zoom** — Scroll-wheel zoom at cursor position. Params: `delta` (float), `cursor_x`, `cursor_y` (pixels).
- **fit_to_window** — Reset zoom to fit image in window.
- **actual_size** — Zoom to 1:1 pixel mapping.
- **fullscreen** — Control fullscreen. Params: `mode` ("on", "off", "toggle").

### Window
- **set_window_geometry** — Set window position/size. Params (all optional): `x`, `y`, `width`, `height`.

### Settings
- **auto_fit_window** — Enable/disable auto-fit. Params: `enabled` (bool).
- **enlarge_small_images** — Enable/disable small image enlargement. Params: `enabled` (bool).

### Utility
- **key** — Simulate a key press. Params: `key` (web convention name).
- **screenshot** — Capture current view as base64 PNG.

## Resources

- **prvw://state** — Current file, zoom, pan, fullscreen, window size, image rendered rect, settings.
- **prvw://menu** — Menu bar structure.
- **prvw://diagnostics** — Cache state, navigation timing, memory usage.

## Simple HTTP endpoints

All endpoints also available as simple HTTP for cURL debugging:

| Method | Path | Body |
|--------|------|------|
| GET | /state | — |
| GET | /menu | — |
| GET | /screenshot | — |
| GET | /diagnostics | — |
| POST | /key | key name |
| POST | /navigate | "next" or "prev" |
| POST | /zoom | "fit", "actual", or float |
| POST | /zoom-in | — |
| POST | /zoom-out | — |
| POST | /scroll-zoom | JSON: `{"delta": 1.0, "cursor_x": 400, "cursor_y": 300}` |
| POST | /fullscreen | "on", "off", "toggle" |
| POST | /open | file path |
| POST | /auto-fit | "on" or "off" |
| POST | /enlarge-small | "on" or "off" |
| POST | /window-geometry | JSON: `{"x": 100, "y": 100, "width": 800, "height": 600}` |
