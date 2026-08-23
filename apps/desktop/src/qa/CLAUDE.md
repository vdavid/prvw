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
- **`menu_text()` is generated, not written down.** It renders the menu bar from `parity::menu_items` for the platform
  the build runs on, so it can't go stale (the hand-kept constant it replaced had, and was missing six items). It's
  `pub(super)` in `http.rs` because `mcp::mcp_resources_read` serves the same text at `prvw://menu`. Shortcuts aren't
  listed: `input::key_to_command` owns the keyboard.
- **`GET /parity`** serves the whole parity table (settings, menu items, commands, each platform's status and any
  `NotApplicable` reason) from `parity::report`. It answers the same on every host, because the registries carry no
  `#[cfg]`.

## Window-chrome diagnostics (debug builds, macOS)

`GET /window-diagnostics` returns a text dump of the main window's AppKit view and layer tree: each titlebar view's
frame in window coordinates, the three standard window buttons, `styleMask`, `collectionBehavior`, and the layer corner
radii/masks. `POST /zoom-window` (native `zoom:`) and `POST /click-zoom-button` (`performClick:` on the green traffic
light) drive the two window-zoom paths without synthesizing OS mouse input. All three are gated by
`#[cfg(all(debug_assertions, target_os = "macos"))]`, like `screenshot_window`.

They exist because window-chrome bugs are invisible to `/state`: the traffic lights' clickable rect drifting away from
their drawing, or a window keeping its fullscreen appearance after a restore, only show up in the AppKit geometry.
`windowed_mode_blocks_appkit_from_starting_a_fullscreen_transition` (in `tests/integration.rs`) reads
`collectionBehavior` out of this dump.

## Browse-mode observability + driving hooks

`GET /state` mirrors the full browse picture (so tests/tools assert it without keystrokes or screenshots): `view_mode`
(`"image"`/`"browse"`), `focused_pane` (`"tree"`/`"grid"`/`"none"`), `browse_selected_folder`, `browse_grid_selected`,
`browse_grid_count` (the listed folder's supported-image count), and `browse_reveal_pending` (the tree's async reveal
walk is in flight — the barrier integration tests poll on before asserting the landed folder/grid).

Three **test-only driving hooks** let integration tests drive browse headlessly, since the QA path can't synthesize a
native outline/collection-view click (and `SendKey` in browse maps only Tab/Enter/Esc — arrows are native):

- `POST /browse/select-folder` (body = absolute path): select a tree folder by path, listing its images into the grid
  (`AppCommand::BrowseSelectFolder`).
- `POST /browse/select-grid` (body = index): select a grid item the way a native click would — updates the grid model so
  the open path reads the right image (`AppCommand::BrowseQaSelectGrid`).
- `POST /browse/open` (no body): open the grid's selected image into image mode (`AppCommand::BrowseOpenSelected`).

All three are macOS-only (browse mode is); off macOS they return 400. Each returns the post-command `/state` snapshot.

## Live-sync observability

`GET /state`'s `watched_folders` lists the folders whose filesystem watch the `folder_watch` worker has **applied**: the
FSEvents stream covering them has started, so changes will be reported. It's not the same as what the app has requested
(`App::watched_folder` / `watched_tree_folders`): watching is asynchronous, and FSEvents reports nothing that happened
before its stream started.

That gap is a flake factory. `/state` answers before the watcher even exists, so a test that mutates a folder right
after startup can get no event at all and then wait out its whole timeout, and the busier the machine the likelier that
is. Every `live_sync_*` test in `tests/integration.rs` polls `TestApp::wait_for_watch` on its folder before touching it.
Do the same in any new live-sync test; a `sleep` here only hides the race.
