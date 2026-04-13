# MCP server

## Purpose

Expose Prvw functionality to AI agents via the Model Context Protocol (MCP). Replaces the old raw-TCP QA server with a
proper Axum-based HTTP server implementing JSON-RPC 2.0.

## Architecture

### Server (`server.rs`)

- Axum HTTP server running in a background tokio task, spawned at app startup
- Binds to `127.0.0.1:19447` by default (localhost only for security)
- `POST /mcp` handles JSON-RPC requests, `GET /mcp/health` returns `{"ok": true}`
- Routes: `initialize`, `tools/list`, `tools/call`, `resources/list`, `resources/read`, `ping`

### Protocol (`protocol.rs`)

- JSON-RPC 2.0 request/response types
- Standard error codes: `INVALID_PARAMS (-32602)`, `INTERNAL_ERROR (-32603)`, `METHOD_NOT_FOUND (-32601)`
- `ServerCapabilities` returned on `initialize` with protocol version `2025-03-26`

### Tools (`tools.rs`)

8 tools: `navigate`, `open`, `zoom`, `fit_to_window`, `actual_size`, `toggle_fullscreen`, `screenshot`, `send_key`.

### Resources (`resources.rs`)

- `prvw://state`: Current file, index, total, zoom, pan, fullscreen, window size (YAML)
- `prvw://diagnostics`: Cache info, preloader status, navigation history (plain text)

Both read from `SharedAppState` (defined in `main.rs`), which the main thread updates on every state change.

### Executor (`executor.rs`)

Routes tool calls to app actions. Most tools emit Tauri events (`qa-navigate`, `qa-open-file`, etc.) that the frontend
handles. `screenshot` loads the current image via `image_loader::load_image()`, encodes to PNG, and returns base64.

### Config (`config.rs`)

Port from `PRVW_MCP_PORT` env var (default 19447, set to 0 to disable).

## Decisions

### Why Axum instead of raw TCP?

The old QA server used raw `TcpListener` with hand-parsed HTTP. Axum adds ~250 KB to the binary but gives proper HTTP
parsing, JSON body extraction, routing, and async support. Worth it for reliability and maintainability.

### Why synchronous executor?

All tools are fire-and-forget (emit Tauri event, return immediately) except `screenshot` which does synchronous image
loading. No need for async round-trips to the frontend yet.

### Why keep SharedAppState in main.rs?

The MCP resources need to read app state, and the main thread needs to update it. Keeping it in `main.rs` avoids
circular dependencies. The MCP module accesses it via `app.try_state::<Mutex<AppState>>()`.

## Gotchas

- **Apple Event handler still uses mpsc channel.** The `macos_open_handler` sends `AppCommand::OpenFile` through an mpsc
  channel, polled by a lightweight thread that emits Tauri events. This is separate from the MCP server.
- **Screenshot loads the full image.** The `screenshot` tool calls `image_loader::load_image()` which decodes the entire
  file. For very large images this may be slow. It doesn't use the preloader cache.
- **`qa-*` event names are retained.** The Tauri events still use `qa-` prefix for frontend compatibility. Renaming
  them would require frontend changes.
