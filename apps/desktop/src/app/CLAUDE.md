# App (core state + event loop)

The `App` struct and its event-loop integration. Everything user-visible flows through
here via `AppCommand`.

| File          | Purpose                                                              |
| ------------- | -------------------------------------------------------------------- |
| `app.rs`      | `App` struct, `App::new`, most methods, `ApplicationHandler` impl    |
| `executor.rs` | `App::execute_command` — the single dispatcher for every `AppCommand` |

## Key patterns

- **Command architecture.** Every user action (keyboard, mouse, menu, QA server, MCP)
  becomes an `AppCommand` (`crate::commands`). `App::execute_command` is the one place
  each command's effect lives. Continuous input (scroll zoom, mouse drag) stays inline
  in the `window_event` handler because it's not a discrete command.
- **Surface lifecycle.** The window + wgpu surface are created in `resumed()`, not at
  startup. Required by winit 0.30 on macOS.
- **Render-on-demand.** `needs_redraw` is set by zoom/pan/resize/navigate. No
  continuous render loop. CPU/GPU usage near zero when idle.
- **Shared state.** `SharedAppState` is a snapshot the QA server reads. Every command
  that mutates observable state calls `update_shared_state()`.
- **Zoom model.** Zoom is absolute (`zoom=1.0` = 1 image pixel per screen pixel).
  `fit_zoom()` is the zoom-to-fit value. `min_zoom` is the floor. `apply_initial_zoom()`
  sets both on image load based on auto-fit + enlarge settings. See the desktop-level
  docs for the full matrix.
- **Title bar + vibrancy.** `content_offset_y()` returns 32px when the title bar
  setting is on and not fullscreen, otherwise 0. Every image-area calculation goes
  through `effective_height()` which subtracts this. Mouse-Y for zoom-at-cursor must
  subtract the offset. `apply_content_offset()` re-applies everything after toggling.

## Adding a new command

See `native_ui/CLAUDE.md` → "How to add a new setting" for the full walkthrough. In
short:

1. Add the variant to `AppCommand` in `crate::commands`.
2. Handle it in `execute_command` (`app/executor.rs`).
3. Map an input to it (`input.rs` for keys/menus, `qa_server.rs` for HTTP/MCP).

## Gotchas

- **`request_inner_size` is async on macOS.** After calling it, `inner_size()` still
  returns the OLD value. `window::resize_to_fit_image` computes and returns the new
  physical size so callers can pass it directly to `renderer.resize()`.
- **Screenshot render differs from main render.** `capture_screenshot` uses a
  stripped path (no viewport offset, no pills, no text) — pixel-based tests of the
  window's visible appearance won't work via screenshot.
- **Global event loop proxy.** `commands::set_event_loop_proxy` is called from
  `resumed()`. Anything non-event-loop (native delegates) uses `commands::send_command`
  via this proxy.
