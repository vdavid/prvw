# Settings

Settings persistence plus the Settings window UI shell. Per-feature panels live with
their feature; this module owns the window chrome, the cross-feature `SettingsDelegate`,
and the "General" panel (which mixes toggles from several features).

| File              | Purpose                                                                            |
| ----------------- | ---------------------------------------------------------------------------------- |
| `persistence.rs`  | `Settings` struct + JSON load/save (override path via `PRVW_DATA_DIR` env var)      |
| `window.rs`       | Window creation, `SettingsDelegate`, sidebar, assembles panels from all features    |
| `widgets.rs`      | `make_setting_row` and `make_wrapping_label` — shared AppKit widget factories       |
| `panels/general.rs` | General panel: Auto-update + Scroll-to-zoom + Title bar (cross-feature toggles)   |
| `panels/raw.rs`   | RAW panel (Phase 3.7 + 6.0 + 6.1): 14 per-stage `RawPipelineFlags` toggles (including the Phase 6.1 "Chroma noise reduction" row under its own "Denoise" section header) + a "Tuning" section with 3 NSSliders (sharpening amount, saturation boost, midtone anchor) + custom DCP dir picker + Reset button |

## Key patterns

- **Retained-mode UI.** Panels are built once. Section switching uses `setHidden:` to
  toggle visibility. Dynamic text (like "scroll to zoom" description) mutates in place
  via stored `NSTextField` pointers in `SettingsDelegateIvars`.
- **Panels live with their feature.** `window.rs` calls
  `crate::zoom::settings_panel::build`, `crate::color::settings_panel::build`,
  `crate::file_associations::settings_panel::build`. The panel functions return
  typed struct (`ZoomPanel`, `ColorPanel`, …) containing the `Retained` widgets the
  delegate needs to wire up.
- **Cross-panel dependencies** (ICC off disables Color match + Relative colorimetric;
  Auto-fit on disables Enlarge) are handled in `SettingsDelegate` methods by
  toggling `setEnabled:` via stored `*const NSSwitch` ivars.
- **Toggles apply immediately** via `AppCommand` through the global event loop proxy.
  No confirm/apply step. The button is "Close".

## Adding a new setting

1. `persistence.rs` — add the field with `#[serde(default)]`, update `Default` + tests.
2. `crate::app::App` struct — add a field, initialize from `initial_settings`.
3. `crate::commands::AppCommand` — add a `Set{Name}(bool)` variant.
4. `app/executor.rs` — handle it: update App field, load/save `Settings`, sync menu
   checkmark if any, call `self.update_shared_state()`.
5. Menu item (optional) — `menu.rs` + `input.rs` + `handle_menu_event`.
6. Settings toggle — add it to the relevant feature's `settings_panel.rs`. If the
   delegate needs to mutate it (cross-dependency), add a field to the panel's output
   struct and plumb the pointer into `SettingsDelegateIvars` in `window.rs`. Wire
   `setTarget`/`setAction` there too.
7. QA/MCP — `features/qa/http.rs` + `features/qa/mcp.rs`.
8. Integration test — `tests/integration.rs`.
