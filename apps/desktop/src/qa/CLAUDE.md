# QA (embedded HTTP server)

An in-process HTTP server for automated QA: used by E2E tests, agent-driven workflows, and MCP clients. Exposes
`GET /state` (and friends) for quick debugging, plus a full MCP JSON-RPC surface at `POST /mcp`.

| File        | Purpose                                                                                    |
| ----------- | ------------------------------------------------------------------------------------------ |
| `server.rs` | Listener loop, request parser, shared utilities (`write_response`, `format_state_json`)    |
| `http.rs`   | Simple HTTP endpoint handlers (`/state`, `/key`, `/zoom`, `/open`, ...)                    |
| `mcp.rs`    | MCP JSON-RPC handler: `handle_mcp`, tools/list, tools/call, resources/list, resources/read |

`server.rs::handle_request` is the only dispatcher. It parses the request line, reads the body, then calls into either
`http::handle_*` or `mcp::handle_mcp`.

## Key patterns

- **Single background thread, no HTTP crate.** We parse requests by hand (`BufReader`, line-based). Keeps deps minimal
  and startup fast.
- **Two interfaces on the same listener.** Simple HTTP (`GET /state`, `POST /key`) for humans and cURL tests; MCP
  JSON-RPC over HTTP for AI-agent clients. Both dispatch to the same `AppCommand` vocabulary.
- **Commands via `EventLoopProxy<AppCommand>`.** Neither `http` nor `mcp` mutates state directly. They send
  `AppCommand`s and read `SharedAppState` snapshots.
- **`Sync` barrier.** For tests that need to know when a command has been processed, `send_and_wait` (MCP, returns
  `Result<Value, Value>`) / `send_and_wait_http` (HTTP, returns `Result<(), String>`) round-trip through the event loop
  and signal on completion.
- **`format_state_json` and `write_response` are shared** utilities in `server.rs`, marked `pub(super)` so both handler
  modules can reach them.
- **Screenshots via offscreen render target.** A separate wgpu render target + buffer readback + PNG encoding. Stripped
  path (no pills, no title bar viewport). Pixel tests of the live window's appearance need a different approach.
- **`screenshot_window` (debug builds only).** Sibling MCP tool that shells out to
  `/usr/sbin/screencapture -l <windowNumber>` to capture the full native window — overlays, title bar, vibrancy, modal
  panels. Compile-time gated by `#[cfg(all(debug_assertions, target_os = "macos"))]` so release binaries neither
  register the tool nor link the dispatch arm. Requires Screen Recording permission; macOS prompts on first invocation.

## Env vars

- `PRVW_QA_PORT`: port to bind (default 19447). `0` disables the server.

## Gotchas

- **Port binding failure is non-fatal.** If the port is taken, the server logs and exits quietly. The viewer keeps
  running.
- **Read timeout = 5 s.** Malformed/stalled connections won't hold up the accept loop.
- **`SharedAppState` lives in `crate::app`**, not here. Imported via `crate::app::SharedAppState`. It's the app-side
  snapshot; we're just a reader.
- **`MENU_TEXT` is `pub(super)` in `http.rs`** because `mcp::mcp_resources_read` also serves it at `prvw://menu`. Kept
  the const inside `http.rs` because the HTTP endpoint is its primary home; MCP reaches across for the same string
  rather than duplicating.
