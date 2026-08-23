# Menu

The menu bar and the right-click context menu, plus the seam for platforms that have neither.

## Layout

- `mod.rs` picks the implementation and documents the API. Nothing else in the app carries a `#[cfg]` for the menu.
- `native.rs` is the muda-backed menu bar, for macOS and Windows. Every item is built from a `MenuItemKey`
  (`crate::parity::menu_items`) through `MenuBuilder`.
- `absent.rs` is the platform with no menu bar (Linux today). `AppMenu` there is an uninhabited enum and
  `create_menu_bar` returns `None`.

`App` holds `Option<AppMenu>` and calls four methods on it: `sync_from_settings`, `set_slideshow_running`,
`set_browse_mode`, and `poll_command`. macOS adds `show_image_context_menu`.

## Decision: muda is a macOS-and-Windows dependency

**Why:** muda's Linux backend is GTK, which is where all nine GTK C crates in `Cargo.lock` and the `apt-get` step in CI
came from. It buys nothing, because muda can only attach a menu bar with `init_for_gtk_window` and winit can't hand it a
`gtk::Window`. Dropping it took 476 lines out of `Cargo.lock`.

**Gotcha:** `default-features = false` alone doesn't work. muda gates its whole Linux backend behind the `gtk` feature
with no fallback, so the crate itself fails to compile there (`E0432: unresolved import self::platform`). It has to
leave the Linux target entirely, which is what the `cfg(any(...))` dependency section in `Cargo.toml` does.

## Decision: state flows one way, settings to menu

**Why:** every checkmark and every enabled state mirrors a field of `Settings`. `sync_from_settings` is the single
idempotent place that maps them, `create_menu_bar` ends with a call to it (items are built unchecked), and every command
that saves one of those settings calls it right after. Nothing else pokes a menu item, so the menu can't drift from the
settings it displays. Same shape as `browser::sync_native`.

Commands run the other way, through `poll_command`. The keyboard's twin table is `input::key_to_command`.

## Decision: items come from the parity registry

**Why:** the menu is one of the surfaces that gets built once per platform, so it's where drift starts. Building an item
means naming a `MenuItemKey`: the key supplies the title (cosmetic shortcut hint included), the id table it registers
turns a click back into a key, and `command_for` matches on that key exhaustively, so a new item can't be added without
deciding what clicking it does. `MenuBuilder::finish` then checks the built set against what `parity::menu_items`
declares for macOS.

This replaced a 31-field `MenuIds` struct and a 30-branch if-else chain, so it's less code than what it guards.

**Windows:** muda builds the same items there, but nothing calls `init_for_hwnd`, so the bar never attaches and the
registry says `Missing` for every item. `finish` skips the audit off macOS for that reason. M4 attaches the bar, flips
the arms to `Present`, and turns the audit on.

## What Linux loses

Deliberate, and it matches the scope decision in `docs/specs/cross-platform-plan.md` (M8: no regressions, no parity
work). Nothing that worked on Linux stopped working, because the menu bar never attached there.

Still reachable, all through `input::key_to_command`: previous/next, first/last, zoom in/out, fit to window, actual
size, fullscreen, histogram, Exif info, loop navigation, slideshow speed, and exit.

Lost with the menu, and worth restoring when Linux gets a spec of its own:

- Sort by name / date / file type.
- Auto-fit window, Enlarge small images.
- ICC color management, Color match display, Relative colorimetric. These three drove the cross-platform color pipeline,
  so this is the most real of the losses.
- Refresh, and start/stop slideshow. Both are menu-only on macOS too; only the slideshow speed keys are bound.

Lost but empty anyway: About, Settings, Copy image, and Print are all no-ops off macOS (`app/executor.rs`).

A future Linux spec owes an in-app menu of some kind. muda can't provide it.
