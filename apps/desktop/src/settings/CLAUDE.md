# Settings

Settings persistence plus the Settings window UI shell. Per-feature panels live with their feature; this module owns the
window chrome, the cross-feature `SettingsDelegate`, and the "General" panel (which mixes toggles from several
features).

| File                | Purpose                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `persistence.rs`    | `Settings` struct + JSON load/save. `data_dir()` picks the per-platform location (`~/Library/Application Support/com.veszelovszki.prvw/`, `%APPDATA%\Prvw\`, or `$XDG_CONFIG_HOME/prvw/` falling back to `~/.config/prvw/`); `PRVW_DATA_DIR` overrides all of it, which the integration tests rely on                                                                                                                                                                                                                                                                                   |
| `window.rs`         | Window creation, `SettingsDelegate`, sidebar, assembles panels from all features                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `widgets.rs`        | `make_setting_row` and `make_wrapping_label` (shared AppKit widget factories)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `panels/general.rs` | General panel: Auto-update + Scroll-to-zoom + Preload next/prev images + Title bar (cross-feature toggles)                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `panels/raw.rs`     | RAW panel (Phase 3.7 + 5.2 + 6.0 + 6.1 + 6.2): 15 per-stage `RawPipelineFlags` toggles (chroma denoise under its "Denoise" section, the Phase 6.2 "Clarity (local contrast)" row atop the "Detail" section) + 7 NSSliders co-located under their matching toggles (baseline exposure offset under baseline exposure, saturation amount under saturation boost, midtone anchor under default tone curve, sharpening amount under capture sharpening, clarity radius + amount under clarity, Phase 5.2 HDR brightness gain under HDR / EDR output) + custom DCP dir picker + Reset button |

## Key patterns

- **Retained-mode UI.** Panels are built once. Section switching uses `setHidden:` to toggle visibility. Dynamic text
  (like "scroll to zoom" description) mutates in place via stored `NSTextField` pointers in `SettingsDelegateIvars`.
- **Panels live with their feature.** `window.rs` calls `crate::zoom::settings_panel::build`,
  `crate::color::settings_panel::build`, `crate::slideshow::settings_panel::build`,
  `crate::file_associations::settings_panel::build`. The panel functions return a typed struct (`ZoomPanel`,
  `ColorPanel`, …) containing the `Retained` widgets the delegate needs to wire up. The sidebar order is General, Zoom,
  Color, RAW, Slideshow, File associations (panel indices 0–5); keep `select_panel`, the `selectXxx:` methods, and
  `switch_settings_section` in sync when adding one.
- **Cross-panel dependencies** (ICC off disables Color match + Relative colorimetric) are handled in `SettingsDelegate`
  methods by toggling `setEnabled:` via stored `*const NSSwitch` ivars. Note: "Enlarge small images" is intentionally
  NOT disabled by "Auto-fit window" — auto-fit is inert in fullscreen, where enlarge still governs small images.
- **Toggles apply immediately** via `AppCommand` through the global event loop proxy. No confirm/apply step. The button
  is "Close".

## Adding a new setting

1. `persistence.rs`: add the field with `#[serde(default)]`, update `Default` + tests.
2. `crate::app::App` struct: add a field, initialize from `initial_settings`.
3. `crate::commands::AppCommand`: add a `Set{Name}(bool)` variant.
4. `app/executor.rs`: handle it: update App field, load/save `Settings`, call `self.sync_menu_from_settings(&s)` if the
   menu mirrors it, call `self.update_shared_state()`.
5. Menu item (optional): `menu/native.rs`, both the item and the `sync_from_settings` line that drives its checkmark.
6. Settings toggle: add it to the relevant feature's `settings_panel.rs`. If the delegate needs to mutate it
   (cross-dependency), add a field to the panel's output struct and plumb the pointer into `SettingsDelegateIvars` in
   `window.rs`. Wire `setTarget`/`setAction` there too.
7. QA/MCP: `features/qa/http.rs` + `features/qa/mcp.rs`.
8. Integration test: `tests/integration.rs`.
