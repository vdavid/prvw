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
- `AppCommand::unimplemented_here` is what `execute_command` consults first: an action the registry calls `Missing` on
  the host is dropped with a log line instead of half-running. That covers every entry point at once (keyboard, menu,
  QA), so per-platform suppression is a coverage arm rather than a `#[cfg]`.
- `menu::native::MenuBuilder::offers` reads both registries to decide whether a platform's menu carries an item at all.

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
- `MenuItemKey`: can a person reach it from a menu here? Both platforms with a bar audit themselves in
  `MenuBuilder::finish`, and `menu::native`'s `what_a_platform_offers_is_what_it_declares` is the static half a Mac can
  run for Windows. Linux says `Missing` throughout because no bar attaches there at all.
- `CommandKey`: does running it do something here? Reachability is the menu's question, not this one.

## What layer 1 doesn't cover yet

Worth knowing before leaning on the table as if it were the whole picture:

- **Keyboard shortcuts.** `input::key_to_command` is still a hand-written table, and accelerators live in `menu::macos`
  and `menu::windows` rather than in `MenuItemKey`. Those two tables check themselves (no two items share a keystroke,
  every mnemonic is unique within its menu), but nothing compares one platform's shortcut set against the other's, so a
  shortcut only one platform carries goes unnoticed here.
- **Surfaces, as opposed to what's inside them.** There's no entry for "the menu bar" or "the settings window" itself,
  which is why Linux shows 34 `Missing` menu items instead of one missing menu bar. Same for the About window,
  onboarding, and browse mode. The launch empty state (`app::EmptyState`) is another one: it's a surface only the
  platforms without onboarding put up, so nothing here can gate a shared E2E test on it.
- **The inverse of a gate.** `SharedApp::start` can say "this test needs X", never "this test needs the platforms
  without X". A behaviour that is a fallback for the platforms missing a feature (image mode standing in for browse mode
  on a folder argument) therefore can't be expressed as a shared test, and lands as unit coverage instead.
- **Where a platform puts things.** `Panel` and `Menu` are the product's shared grouping, and they still say `App` for
  the six items Windows scatters (About to Help, Settings to Tools, Quit to File). Windows placement is data, but it's
  `menu::windows`'s `WindowsMenu` rather than anything here, so the parity table's group column reads macOS's answer for
  every platform.

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
