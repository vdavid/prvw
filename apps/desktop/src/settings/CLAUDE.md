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

`Settings` (`persistence.rs`) is the source of truth for what a setting **is**. `SettingKey`
(`crate::parity::setting_keys`) is the source of truth for what a **UI owes it**. Keep that line clean: no second copy
of the data model in the registry, and no settings row anywhere that isn't named by a key.

1. `persistence.rs`: add the field with `#[serde(default)]`, update `Default` + tests.
2. `parity/setting_keys.rs`: add a `SettingKey` variant with its label, panel, control kind, and the `Settings` field it
   drives. Skipping this fails `every_settings_field_has_a_key`, which walks the serialized `Settings` and reports any
   field no key claims.
3. Answer for **every platform**. The new variant breaks `macos_coverage`, `windows_coverage`, and `linux_coverage`
   until each says `Present`, `NotApplicable { reason }`, or `Missing`. That's the parity guarantee; `cargo check` here
   catches the Windows arm too, and `./scripts/check.sh --check windows-cross --check linux-cross` compiles both.
4. `crate::app::App` struct: add a field, initialize from `initial_settings`.
5. `crate::commands::AppCommand`: add a `Set{Name}(bool)` variant, and give it a `CommandKey` in `parity_key` (or
   declare it plumbing, which a setting never is).
6. `app/executor.rs`: handle it: update App field, load/save `Settings`, call `self.sync_menu_from_settings(&s)` if the
   menu mirrors it, call `self.update_shared_state()`.
7. Menu item (optional): add a `MenuItemKey`, build it in `menu/native.rs` through `MenuBuilder`, and add the
   `sync_from_settings` line that drives its checkmark.
8. Settings row: call `make_setting_row(audit, SettingKey::Yours, description, ...)` from the relevant panel. The title
   comes from the key; the description stays an argument, because that copy talks about the platform's own hardware and
   conventions ("wide-gamut (P3) screens like MacBooks"). If the delegate needs to mutate the widget (cross-dependency),
   add a field to the panel's output struct and plumb the pointer into `SettingsDelegateIvars` in `window.rs`. Wire
   `setTarget`/`setAction` there too.
9. QA/MCP: `qa/http.rs` + `qa/mcp.rs`.
10. E2E test: `tests/e2e_shared.rs` if the setting's behaviour is observable through `/state` on any platform (name the
    `CommandKey` in `SharedApp::start` and let the registry decide who runs it), `tests/e2e_macos.rs` if asserting it
    means poking the AppKit form.

### Why a row can't skip the registry

The row factories take a `SettingKey` rather than a title string, so building a row registers it. `check_parity` in
`window.rs` compares what the window built against what macOS declared, and `settings_opens_and_closes` (which opens the
real window) is where a mismatch surfaces: a `Present` nobody built panics with the key's name. The compiler can't see
inside a widget factory, so this is the half that catches a declaration nobody honoured.

### The `NotApplicable` escape hatch

`Coverage::NotApplicable { reason }` is for a setting that is **meaningless** on a platform, and the reason is data:
layer 2 renders it in `docs/parity.md`, so a reader can judge it. `SettingKey::TitleBar` is the shape to copy: macOS
draws content behind a transparent title bar, a Win32 client area starts below the caption, so there is nothing there to
reserve space for.

**Using it without a real reason is how this whole layer rots.** A reason like "n/a" or "doesn't fit our UI" turns a
compile error into a shrug, and the next person copies the shrug. Two rules keep it honest:

- Name the platform fact that makes the setting meaningless: a window model, an OS convention, an API that doesn't
  exist. If the sentence would work for any setting, it isn't a reason.
- "We haven't built it yet" is `Missing`, never `NotApplicable`. `Missing` is what the parity table counts as a gap, and
  the point is that the gaps stay countable.
