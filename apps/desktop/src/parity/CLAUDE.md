# Parity

The registries that make "does every platform have this?" a question the build answers. Layer 1 of the M0.5 parity
harness in `docs/specs/cross-platform-plan.md`.

| File              | Purpose                                                                         |
| ----------------- | ------------------------------------------------------------------------------- |
| `mod.rs`          | `Platform`, `Coverage`, `Audit`, and `report()` (the whole table, for layer 2)  |
| `setting_keys.rs` | `SettingKey`: every persisted setting, plus each platform's coverage            |
| `menu_items.rs`   | `MenuItemKey`: every menu item, its title, and the action a click runs          |
| `command_keys.rs` | `CommandKey`: every user-invocable action, and whether a platform implements it |

## Decision: the registries carry no `#[cfg]`

**Why:** every platform's coverage compiles on every host. A `cargo check` on a Mac catches a Windows arm nobody filled
in, and one host can generate the whole parity table rather than the slice it happens to build. The `#[cfg]`s stay in
the UI modules. The one conditional thing is a dead-code allow on `mod parity;` in `main.rs`, for the platforms whose UI
doesn't consume the registries yet.

The tree is also dependency-free (`core` and `std` only), which is what lets `tests/parity_registries.rs` load it with
`#[path]` and compile the fixtures in about a tenth of a second.

## Decision: the entries are load-bearing, not bookkeeping

**Why:** a registry nobody consults rots into fiction within a month, and then it's worse than nothing because it looks
authoritative. So the UI can't be built without it:

- `settings::widgets::make_setting_row` and the RAW panel's row factories take a `SettingKey` and get the row's title
  from it.
- `menu::native::MenuBuilder` builds every item from a `MenuItemKey`, whose `title` the item wears.
- Both record what they built into an `Audit`, which is compared against the declaration as the UI is assembled.
- `command_for` in `menu/native.rs` checks each item against the action `MenuItemKey::command` registered for it.
- `AppCommand::log_if_unimplemented` says so in the log when an action the registry calls `Missing` gets run anyway.

## What each layer of the guarantee catches

1. **Compile time:** a platform that doesn't answer for an entry. `tests/parity_registries.rs` proves it, with one
   `E0004` per registry.
2. **Test time:** a `Settings` field with no key (`every_settings_field_has_a_key`). The compiler can't see a new struct
   field, so this is what starts the chain.
3. **Run time:** a `Present` the UI never built, or a widget the registry doesn't know about (`Audit::mismatches`, via
   `settings::window::check_parity` and `MenuBuilder::finish`). `settings_opens_and_closes` exercises the first one.

## Gotcha: coverage answers a different question per registry

- `SettingKey`: is the setting exposed anywhere on this platform? Panel rows and menu-only settings both count, which is
  why `SettingKey::panel_coverage` filters to `Panel::None` before an audit compares it against a settings window.
- `MenuItemKey`: can a person reach it from a menu here? muda builds the items on Windows too, but nothing calls
  `init_for_hwnd`, so nothing is reachable and the arms say `Missing`. `MenuBuilder::finish` only audits on macOS for
  that reason; M4 attaches the bar, flips the arms, and turns the audit on.
- `CommandKey`: does running it do something here? Reachability is the menu's question, not this one.

## What layer 1 doesn't cover yet

Worth knowing before leaning on the table as if it were the whole picture:

- **Keyboard shortcuts.** `input::key_to_command` is still a hand-written table, and accelerators live in the muda calls
  in `menu/native.rs` rather than in `MenuItemKey`. Windows will want `Ctrl` where macOS uses `Cmd` (muda's
  `Modifiers::SUPER` is the Windows key there), so shortcuts are a per-platform decision M4 has to make deliberately.
  Nothing here catches a shortcut one platform is missing.
- **Surfaces, as opposed to what's inside them.** There's no entry for "the menu bar" or "the settings window" itself,
  which is why Windows shows 36 `Missing` menu items instead of one missing menu bar. Same for the About window,
  onboarding, and browse mode.
- **Where a platform puts things.** `Panel` and `Menu` are the product's shared grouping. A platform that has to place
  an item natively somewhere else (Windows convention puts About under Help) says so in its coverage arm's comment
  today; if that becomes common, placement belongs in the coverage data.

## Layer 2: the generated table

`report()` returns the table as owned data: registry, name, label, group, control kind, backing field, and every
platform's status with its `NotApplicable` reason. It's a pure function of the registries, so it answers the same on
every host. Three things read it:

- **`docs/parity.md`**, rendered by `cargo xtask parity` and kept honest by the `parity` check
  (`scripts/check/checks/parity-table.go`), which rewrites it locally and fails in CI on a stale file. Never edit that
  file by hand; edit an arm here and regenerate.
- **`GET /parity`** on the QA server, the same thing as JSON.
- Nothing else. If you want a fourth consumer, take it from `report()` rather than re-reading the registries.

A coverage arm you flip shows up as a diff in `docs/parity.md`, which is the point: parity moves in review, not
silently. `xtask/CLAUDE.md` covers why the generator is its own dependency-free crate.
