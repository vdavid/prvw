# QA (embedded HTTP server)

An in-process HTTP server for automated QA: used by E2E tests, agent-driven workflows,
and MCP clients. Exposes `GET /state` (and friends) for quick debugging, plus a full
MCP JSON-RPC surface at `POST /mcp`.

| File       | Purpose                                                                  |
| ---------- | ------------------------------------------------------------------------ |
| `server.rs` | Raw `TcpListener` loop, HTTP parsing, MCP handler, `SharedAppState`, screenshot path |

## Key patterns

- **Single background thread, no HTTP crate.** We parse requests by hand (`BufReader`,
  line-based). Keeps deps minimal and startup fast.
- **Two interfaces on the same listener.** Simple HTTP (`GET /state`, `POST /key`) for
  humans and cURL tests; MCP JSON-RPC over HTTP for AI-agent clients. Both share the
  same `AppCommand` dispatch.
- **Commands via `EventLoopProxy<AppCommand>`.** The server never mutates state
  directly. It sends `AppCommand`s and reads `SharedAppState` snapshots.
- **`Sync` barrier.** For tests that need to know when a command has been processed,
  `AppCommand::Sync(tx)` round-trips through the event loop and signals completion.
- **Screenshots via offscreen render target.** A separate wgpu render target + buffer
  readback + PNG encoding. Stripped path (no pills, no title bar viewport) — pixel
  tests of the live window's appearance need a different approach.

## Env vars

- `PRVW_QA_PORT` — port to bind (default 19447). `0` disables the server.

## Gotchas

- **Port binding failure is non-fatal.** If the port is taken, the server logs and
  exits quietly. The viewer keeps running.
- **Read timeout = 5 s.** Malformed/stalled connections won't hold up the accept loop.
- **`SharedAppState` lives here for now.** Earlier critique said it arguably belongs in
  `app::state`. Moved last; we kept it colocated with the server because the server is
  its only reader.
