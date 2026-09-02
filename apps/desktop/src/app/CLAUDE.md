# App (infrastructure: core state + event loop)

Not a feature. This is the runtime scaffolding every feature plugs into.

| File              | Purpose                                                          |
| ----------------- | ---------------------------------------------------------------- |
| `app.rs`          | `App` struct, `App::new`, `ApplicationHandler` impl              |
| `executor.rs`     | `App::execute_command`: single dispatcher for every `AppCommand` |
| `shared_state.rs` | `SharedAppState` snapshot + `App::update_shared_state` writer    |

## App's fields

App holds three per-feature State structs (`zoom`, `color`, `navigation`) plus truly cross-cutting state:

- **Per-feature state**: `zoom: zoom::State`, `color: color::State`, `navigation: navigation::State`,
  `histogram: histogram::State`, `exif_overlay: exif_overlay::State`, `slideshow: slideshow::State`,
  `browser: browser::State` (browse mode — `ViewMode`, `focused_pane`, tree selection, grid selection, native handles).
  Each feature's runtime + setting-backed fields live in its own module.
- **Launch**: `file_path`, `explicit_files`, `waiting_for_file`, `launch_directory` (a lone directory CLI arg → browse
  mode on macOS and Windows, the folder's images in image mode on Linux; see `browser::classify_launch_target`,
  `launch`, and `initialize_viewer`), `wait_start`, `empty_state` (why image mode is showing no image, if it isn't).
  `file_path` is what opens (argv order, so `prvw b.png a.png` opens b.png) while `explicit_files` becomes a list in the
  user's **sort** order, so `initialize_viewer` positions the list at `file_path`. Everything downstream keys off
  `dir_list.current_index()` — the cache slot, the title's `n / total`, the preload window — so a list sitting anywhere
  else shows a blank window and mislabels it.
- **Handles**: `window`, `renderer`, `app_menu`.
- **Cross-cutting toggle**: `title_bar` (affects window chrome, not enough to justify its own feature state struct).
- **Runtime input**: `modifiers`, `drag_start`, `last_mouse_pos`, `last_click_time`, `scroll` (`crate::scroll::Scroll` —
  the platform's zoom modifier plus the running delta-to-images conversion), `needs_redraw`, `scale_factor`,
  `pending_drops` + `files_hovering` (a drop in progress: winit reports one path per event and nothing to mark the end
  of the batch, so the paths pile up and `about_to_wait` opens them as one request — see `App::open_dropped`).
- **Cross-thread**: `shared_state`, `event_loop_proxy`, `_qa_handle`.
- **Folder reading**: `folder_scanner` (the one `folder_scan::FolderScanner`), plus who's waiting on a result:
  `pending_grid_listing`, `pending_rescan`, `pending_modified`, and `navigation.scan_pending` for image mode.
  `App::handle_folder_scanned` routes one `AppCommand::FolderScanned` to all of them.

App doesn't implement any feature's logic. The handler arms in `execute_command` mutate `self.zoom`, `self.color`,
`self.navigation` fields or delegate to the feature (e.g. `window::set_fullscreen`,
`crate::settings::show_settings_window`).

## Key patterns

- **Surface lifecycle.** The window + wgpu surface are created in `resumed()`, not at startup. Required by winit 0.30 on
  macOS.
- **Render-on-demand.** `needs_redraw` is set by zoom/pan/resize/navigate. No continuous render loop.
- **No directory reads on the main thread.** Every `read_dir` goes to `folder_scan::FolderScanner`; the result arrives
  as one `AppCommand::FolderScanned` that `handle_folder_scanned` fans out. Launch and `OpenFile` show the image against
  a provisional one-file list and swap in the real folder when the scan lands.
- **Shared-state boundary.** Main thread writes `SharedAppState` on every state change. QA thread reads under
  `Arc<Mutex<_>>`. Diagnostics text is computed via `crate::diagnostics::build_text` and stored in the snapshot.
- **Commands bridge features and App.** `AppCommand::*` arrives in `execute_command`; the handler mutates App / feature
  State fields (`self.zoom.auto_fit`, `self.color.icc_enabled`, `self.title_bar`, etc.) or delegates to the feature.

## Adding a new command

1. Add the variant to `crate::commands::AppCommand`.
2. Handle it in `app/executor.rs`: mutate the relevant `self.<feature>.<field>` or App field, call
   `update_shared_state()` if the change is observable.
3. Map input to the command somewhere (`crate::input` for keys/menus, `crate::qa::http` or `crate::qa::mcp` for
   HTTP/MCP).

## Decision: per-feature State structs

**Decision:** Each feature owns its runtime state (`zoom::State`, `color::State`, `navigation::State`) rather than flat
fields on `App`. App holds the struct as a field.

**Why:** Lets features grow state without bloating App. State is physically close to the code that reads/writes it.
Visibility boundary is natural: external code goes through `App.feature.field`, not a grab bag of flat fields.

**How to apply:** When you need state for a new feature, decide:

- **Multiple fields that cohere** (e.g. a feature with 3+ settings) → add a `State` struct in the feature module and a
  field on App.
- **Single bool that's globally read** (e.g. `title_bar`) → plain field on App.
