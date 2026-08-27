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
- **`screenshot_window` (debug builds only).** Sibling MCP tool that photographs the whole native window: overlays,
  title bar, window chrome, modal panels. `qa/window_capture.rs` owns it, gated by
  `#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "windows")))]` so release binaries neither register
  the tool nor link the dispatch arm. macOS shells out to `/usr/sbin/screencapture -l <windowNumber>` and needs Screen
  Recording permission (macOS prompts on first invocation); Windows calls `PrintWindow` through
  `platform::windows::window_capture` and needs no permission. Both hand back PNG bytes, and `mcp_image_content` in
  `mcp.rs` is the single place the base64 contract is built, shared with `screenshot`.

**Which of the two a QA task wants.** `screenshot` is portable and exact about the image, and shows nothing else, so it
answers "did the right pixels get decoded and transformed?". `screenshot_window` is the only one that can answer "does
it look right?", because everything a person reads off the screen (the zoom pill, the EXIF panel, the histogram, the
title strip, the frame around it) lives outside the offscreen render target. A wgpu surface readback would land between
the two: it would pick up the overlays but still miss the chrome the window manager draws, so it isn't a replacement for
either.

## The E2E suite this server exists for

The suite drives the app through this server rather than through the UI, which is what lets the same assertions run on
every platform (layer 3 of the parity harness, `docs/specs/cross-platform-plan.md` M0.5). Three pieces:

- `tests/e2e/` — the harness. `TestApp` spawns the binary and talks to this server, `fixtures` generates the images,
  `shared` holds the gate. Compiles everywhere.
- `tests/e2e_shared.rs` — the platform-neutral core, 41 tests. No `cfg` anywhere in it.
- `tests/e2e_macos.rs` — 18 tests that poke a native widget: browse mode, the settings window, the AppKit fullscreen
  round trip, `screenshot_window`.

**A shared test can't reach a `TestApp` directly.** It goes through `SharedApp::start`, naming the `CommandKey`s it
exercises, and the gate resolves them against `GET /parity` — the same registries layer 1 checks at compile time. `done`
runs the test, `not applicable` skips it with the registry's reason, `missing` fails it by name. So a behaviour a
platform hasn't built shows up as a red test rather than a quiet pass, and a test skipped off macOS carries the sentence
explaining why. Adding a shared test means naming what it needs; `shared_suite_stays_platform_neutral` rejects
`target_os` and a raw `TestApp` in that file.

A skipped shared test reports as passing (Rust has no skip) and writes a `SKIP <test>: <reason>` line to stderr, so
`cargo nextest run --no-capture` is how you read them and `docs/parity.md` is how you predict them.

**What is proven, and what isn't.** Only macOS has ever run these. Windows and Linux type-check and lint them
(`./scripts/check.sh --check windows-cross` / `--check linux-cross` build `--all-targets`), which is a real guarantee
about the harness compiling and a claim about nothing else. The Linux job skips the shared suite while its runner has no
`DISPLAY`, because the app can't open a window there; give the job a session and it runs. The waits are all tuned to
macOS. `tests/e2e/mod.rs` carries the full caveat list.

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
- **`content_offset_y` in `/state`** is the logical pixels reserved at the top before content starts, from
  `App::content_offset_y`. It's non-zero only where the window draws behind its own title bar, so macOS today. Overlay
  geometry hangs off it, which is why it's in the contract: a test aiming at the histogram reads it rather than assuming
  a platform's value.
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
their drawing, or a window keeping its fullscreen appearance after a restore, only show up in the AppKit geometry. No
E2E test reads the dump today; `fullscreen_state_survives_a_round_trip_appkit_drove` (in `tests/e2e_macos.rs`) covers
the restore case through `/state` instead.

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

`POST /show-about` is a fifth, and it runs wherever `CommandKey::About` is `Present`. It waits for the event loop to
acknowledge the command, which is the whole point: a box that opened a message loop of its own would hold the reply, so
the endpoint answering at all is the assertion. `about_opens_without_holding_up_the_app` in `tests/e2e_shared.rs` is the
caller.

All three run wherever browse mode has a UI, which is macOS and Windows; elsewhere they return 400. Each returns the
post-command `/state` snapshot.

`POST /drop` (body = one absolute path per line) is the fourth of the same kind, and it runs everywhere: a real drop is
an OS drag session no HTTP request can synthesise, so this hands the app the path list winit would deliver one
`DroppedFile` at a time (`AppCommand::OpenDropped`). What the paths mean is `launch::classify_open_request`.

## Live-sync observability

`GET /state`'s `watched_folders` lists the folders whose filesystem watch the `folder_watch` worker has **applied**: the
FSEvents stream covering them has started, so changes will be reported. It's not the same as what the app has requested
(`App::watched_folder` / `watched_tree_folders`): watching is asynchronous, and FSEvents reports nothing that happened
before its stream started.

That gap is a flake factory. `/state` answers before the watcher even exists, so a test that mutates a folder right
after startup can get no event at all and then wait out its whole timeout, and the busier the machine the likelier that
is. Every `live_sync_*` test polls `TestApp::wait_for_watch` on its folder before touching it. Do the same in any new
live-sync test; a `sleep` here only hides the race. (`folder_watch` rides the `notify` crate, so the backend is
FSEvents, `ReadDirectoryChangesW`, or inotify depending on the host. The race and the barrier are the same on all three;
only the latency differs.)

The list is only worth blocking on if it can never name a dead watch, so `folder_watch::record_watch_outcome` arms a
folder on exactly one outcome — a `Watch` that `notify` accepted — and disarms it on every other, a failed re-watch of
an already-armed folder included.
