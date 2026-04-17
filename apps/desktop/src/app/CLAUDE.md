# App (infrastructure — core state + event loop)

Not a feature — this is the runtime scaffolding every feature plugs into.

| File              | Purpose                                                                 |
| ----------------- | ----------------------------------------------------------------------- |
| `app.rs`          | `App` struct, `App::new`, `ApplicationHandler` impl                     |
| `executor.rs`     | `App::execute_command` — single dispatcher for every `AppCommand`       |
| `shared_state.rs` | `SharedAppState` snapshot + `App::update_shared_state` writer           |

## App's fields

App holds three per-feature State structs (`zoom`, `color`, `navigation`) plus
truly cross-cutting state:

- **Per-feature state**: `zoom: zoom::State`, `color: color::State`,
  `navigation: navigation::State`. Each feature's runtime + setting-backed fields
  live in its own module.
- **Launch**: `file_path`, `explicit_files`, `waiting_for_file`, `wait_start`.
- **Handles**: `window`, `renderer`, `app_menu`.
- **Cross-cutting toggle**: `title_bar` (affects window chrome, not enough to
  justify its own feature state struct).
- **Runtime input**: `modifiers`, `drag_start`, `last_mouse_pos`, `last_click_time`,
  `needs_redraw`, `scale_factor`.
- **Cross-thread**: `shared_state`, `event_loop_proxy`, `_qa_handle`.

App doesn't implement any feature's logic — the handler arms in `execute_command`
mutate `self.zoom`, `self.color`, `self.navigation` fields or delegate to the
feature (e.g. `window::toggle_fullscreen`, `crate::settings::show_settings_window`).

## Key patterns

- **Surface lifecycle.** The window + wgpu surface are created in `resumed()`, not
  at startup. Required by winit 0.30 on macOS.
- **Render-on-demand.** `needs_redraw` is set by zoom/pan/resize/navigate. No
  continuous render loop.
- **Shared-state boundary.** Main thread writes `SharedAppState` on every state
  change. QA thread reads under `Arc<Mutex<_>>`. Diagnostics text is computed via
  `crate::diagnostics::build_text` and stored in the snapshot.
- **Commands bridge features and App.** `AppCommand::*` arrives in `execute_command`;
  the handler mutates App / feature State fields (`self.zoom.auto_fit`,
  `self.color.icc_enabled`, `self.title_bar`, etc.) or delegates to the feature.

## Adding a new command

1. Add the variant to `crate::commands::AppCommand`.
2. Handle it in `app/executor.rs`: mutate the relevant `self.<feature>.<field>` or
   App field, call `update_shared_state()` if the change is observable.
3. Map input to the command somewhere (`crate::input` for keys/menus, `crate::qa::http`
   or `crate::qa::mcp` for HTTP/MCP).

## Decision — per-feature State structs

**Decision:** Each feature owns its runtime state (`zoom::State`, `color::State`,
`navigation::State`) rather than flat fields on `App`. App holds the struct as a
field.

**Why:** Lets features grow state without bloating App. State is physically close
to the code that reads/writes it. Visibility boundary is natural — external code
goes through `App.feature.field`, not a grab bag of flat fields.

**How to apply:** When you need state for a new feature, decide:
- **Multiple fields that cohere** (e.g. a feature with 3+ settings) → add a
  `State` struct in the feature module and a field on App.
- **Single bool that's globally read** (e.g. `title_bar`) → plain field on App.
