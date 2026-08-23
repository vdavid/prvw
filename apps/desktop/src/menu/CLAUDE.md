# Menu

The menu bar and the right-click context menu, plus the seam for platforms that have neither.

## Layout

- `mod.rs` picks the implementation and documents the API. Nothing else in the app carries a `#[cfg]` for the menu.
- `native.rs` is the muda-backed menu bar, for macOS and Windows. Every item is built from a `MenuItemKey`
  (`crate::parity::menu_items`) through `MenuBuilder`.
- `macos.rs` and `windows.rs` are the two decoration tables: what an item is called on that platform and what shortcut
  it carries. `native.rs` aliases one of them as `chrome`.
- `windows.rs` also puts the bar on the window and takes it away for fullscreen. That half is `#[cfg]`-gated; the table
  half is not.
- `absent.rs` is the platform with no menu bar (Linux today). `AppMenu` there is an uninhabited enum and
  `create_menu_bar` returns `None`.

`App` holds `Option<AppMenu>` and calls five methods on it: `sync_from_settings`, `set_slideshow_running`,
`set_browse_mode`, `set_fullscreen`, and `poll_command`. macOS adds `show_image_context_menu`.

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
means naming a `MenuItemKey`: the key supplies the label, the id table it registers turns a click back into a key, and
`command_for` matches on that key exhaustively, so a new item can't be added without deciding what clicking it does.
`MenuBuilder::finish` then checks the built set against what `parity::menu_items` declares for the host, on both
platforms that have a bar.

This replaced a 31-field `MenuIds` struct and a 30-branch if-else chain, so it's less code than what it guards.

`what_a_platform_offers_is_what_it_declares` is the static half of that audit, and the only half a Mac can run for
Windows: it checks the filter's answer against the registry's for both platforms at once.

## Decision: one shared label, two tables of decoration

**Why:** the thing a menu item _is_ has one name (`MenuItemKey::label`), and it is the same string everywhere, so the
parity audit can compare by key and a rename can't happen on one platform only. What an item _looks like_ is not shared
at all: macOS pads a cosmetic shortcut hint into the title and binds Command; Windows marks the mnemonic Alt underlines
with `&`, right-aligns a tab-separated shortcut column, and binds Ctrl.

So `macos.rs` and `windows.rs` own titles and accelerators, and no call site in `native.rs` spells either: `build.item`
asks `chrome`. Windows renames exactly one thing, File → Exit, and it says so in one place.

**Both tables compile on both platforms**, for the same reason `parity/` does: a `cargo test` on a Mac runs the Windows
table's tests (mnemonic uniqueness per menu, no two items sharing a shortcut, every offered item dressed), and a Windows
build type-checks the macOS one. Only the Win32 code inside `windows.rs` is gated. The price is a `dead_code` allow on
each module declaration in `mod.rs`.

**The hint belongs to the item's own name.** The two items whose label flips with a mode compose their own title
(`browse_toggle_title`, `slideshow_toggle_title`), because whether a mode advertises the key is a product question: `s`
starts and stops a slideshow alike, so both names carry it, while Enter only takes you _into_ the browser (the focused
pane owns it once you are there), so "Image view" carries nothing.

## Decision: a platform gets an item only where the registry says it works

**Why:** a menu that lists something a platform hasn't built is worse than one that doesn't, because a dead item reads
as a bug in the app rather than a gap. `MenuBuilder::offers` decides, from two registries: an item the menu registry
calls `NotApplicable` has no meaning here, and an item whose action `parity::command_keys` calls `Missing` would
dispatch a command `execute_command` drops. `fill` then drops the separators a filtered item would strand, and a submenu
the filter emptied never joins the bar (macOS's Help excepted: AppKit fills it itself).

So suppressing a feature on a platform means flipping a coverage arm and watching `docs/parity.md` move; there is no
`#[cfg]` in this file for it. The Windows bar is the live demonstration: it ships File, View, Navigate, and Slideshow,
and shows no Edit, Tools, or Help menu at all, because Copy image, Settings, and About are the actions Windows hasn't
built yet (M1 step 12, M4, M6). Each one comes back by building the thing, not by editing this file.

Fields on `AppMenu` are `Option<T>` for the same reason: `set_checked` / `set_enabled` write through, and `dispatch`
uses `as_ref()?`. `macos_offers_every_item` is the guard that stops the filter from ever thinning the Mac menu bar.

## Decision: Windows scatters the app menu rather than keeping one

**Why:** there is no app menu on Windows, so macOS's six app-menu items go where Windows users look for them: About to
Help as its only and therefore last item, Settings to Tools, Quit to the bottom of File as Exit. Hide / Hide others /
Show all are `NotApplicable` — Windows minimizes windows instead — and Close window joins them, because Prvw has one
window there and an app with no windows is an invisible process rather than a running app.

That placement is the **only** structural difference between the two bars, and it is the two `#[cfg]` blocks near the
top of `create_menu_bar`. Everything below them (which menus exist, what is in them, the order, the separators) is one
definition. `parity::menu_items::Menu` stays the product's shared grouping and still says `App`; `menu::windows`'s own
`WindowsMenu` is where Windows placement lives.

## Decision: the menu bar goes away in fullscreen, on Windows

**Why:** fullscreen is where the image really is the whole app, and no Windows app shows a menu bar there. There's no
auto-hide and no setting for it in v1: Explorer's Alt-reveal is discoverable only by accident, the menu is the only
mouse path to most features until browse mode ships, and F11 is already the escape hatch. `AppMenu::set_fullscreen` does
it through muda's `hide_for_hwnd` / `show_for_hwnd`, which keep muda's window subclass in place, so accelerators (F11
among them) keep working while the bar is gone. macOS hides its own bar, so it's a no-op there.

`App::set_fullscreen` (`app/executor.rs`) is the single route into `window::set_fullscreen`, which is what keeps the bar
in step with it. A second route would leave a menu bar sitting over a fullscreen image.

## Gotcha: muda's accelerators do nothing without a message hook

**Why:** muda's own docs say it: "For accelerators to work, the event loop needs to call `TranslateAcceleratorW` with
the `HACCEL` returned from `Menu::haccel`", and winit's Windows loop doesn't. Without it Ctrl+O, Ctrl+=, Ctrl+-, F11,
and F5 all silently do nothing. `platform::windows::msg_hook` is that call, and its module docs carry the ordering rule
it shares with M4's dialogs and the reason it passes the main window's HWND rather than `msg.hwnd`.

`menu::windows::attach` reads `Menu::haccel()` **on every message** rather than caching it: muda destroys and recreates
the accelerator table whenever an item joins or leaves a menu, so a stored handle can outlive what it names.

## Gotcha: a menu has to join the bar before it is filled

muda registers an item's accelerator into the **root** menu's `HACCEL` table as the item is appended (`AccelAction::add`
walks the child's `root_menu_haccel_stores`), and a submenu that hasn't joined a root yet has no table to register into.
Appending the submenu afterwards doesn't go back for its children, so filling first leaves every accelerator in the bar
dead — while the items still _show_ their shortcut, because the text is composed on a different path. That silence is
the whole failure mode.

`top_level` is what keeps the order right: it appends each top-level menu to the bar at creation, and the menus the
filter empties come back off at the end. The Sort by submenu is the one that still joins its parent through `fill`, so
it can't carry accelerators; `the_sort_by_submenu_carries_no_accelerators` is the guard.

## Gotcha: muda puts Ctrl+Q on File → Exit and won't take it off

`PredefinedMenuItem::quit` carries a built-in `CMD_OR_CTRL+Q` accelerator on Windows, and there is no setter to remove
it, so the item shows a shortcut where `docs/specs/windows-ui-design.md` asked for none. Ctrl+Q is a real quit
convention on Windows (Qt binds it by default), so it stays rather than costing us the toolkit's own Quit item. The
alternative is a plain `MenuItem` dispatching `AppCommand::Exit`, which would make `MenuItemKey::Quit` claim an action
it doesn't have on macOS.

## Gotcha: winit already accounts for the menu bar's height

`WindowFlags::adjust_rect` passes `GetMenu(hwnd) != 0` to `AdjustWindowRectEx`, so `request_inner_size` and
`resize_to_fit_image` land on the right client size once the bar is up. That's why `create_menu_bar` is called
**before** the renderer in `initialize_viewer`: attaching the bar takes its height out of the client area, and doing it
after the wgpu surface exists would size the surface twice. The one case Win32 can't answer for is a bar that wraps to
two lines in a very narrow window, which `AdjustWindowRectEx` is documented not to handle.

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
