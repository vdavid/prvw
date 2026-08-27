# About Prvw

One box per platform, one set of strings. `content.rs` is what it says; the platform file beside it is how it looks.

| File         | Purpose                                                                   |
| ------------ | ------------------------------------------------------------------------- |
| `content.rs` | What the box says, as data. Compiles and is tested on every host          |
| `macos.rs`   | A non-modal `NSWindow`, opened from the Prvw menu or Cmd+Shift+A          |
| `windows.rs` | A modeless Win32 popup under Help → About Prvw, plus its window procedure |

Linux has neither: there's no menu bar to open one from (`menu/absent.rs`), so `CommandKey::About` is `Missing` there.

## Decision: the copy is shared, the layout is not

**Decision:** every user-visible string lives in `content.rs`, and `AboutContent::for_platform` takes the platform
rather than reading `cfg!`.

**Why:** the chrome forks by OS (`docs/design-principles.md`), and the one thing that must not fork is what the box
claims about the product. Taking the platform as an argument is what lets a Mac assert Windows' copy: `src/scroll.rs`
and `src/paths.rs` are the precedent for holding per-platform policy as data. It's also what makes the style-guide test
possible, which checks all three platforms' strings for an em dash and the trivializing words in one pass. This
milestone is almost entirely copy, so that guard earns its place.

Only the tagline actually forks, and it forks because naming the wrong operating system in the product's own About box
is the kind of detail a ported app gets wrong.

## Decision: Windows shows a licence line and macOS doesn't

**Decision:** `content` carries the licence sentence for every platform; only `windows.rs` renders it.

**Why:** `docs/specs/windows-ui-design.md` asks for it on Windows, and the macOS box is shipping UI that David reviews
before it changes. So the asymmetry is deliberate and it's parked here rather than hidden: adding the line to macOS is
one `make_label` in `macos.rs`, whenever he wants it.

## Decision: a plain popup, not a task dialog and not a dialog template

**Decision:** `windows.rs` creates a `WS_POPUP | WS_CAPTION | WS_SYSMENU` window and its controls in code, then
registers it with `platform::windows::msg_hook` so `IsDialogMessageW` runs first for its messages.

**Why:** three things had to be true at once.

- **No nested message loop.** `TaskDialogIndirect` and `DialogBoxParam` both run one, and on Windows that starves
  winit's pump rather than crashing: `about_to_wait` stops running, so the slideshow timer freezes behind the box. The
  rule is in `AGENTS.md` and the mechanism is in `platform/windows/msg_hook.rs`.
- **Dark mode.** Task dialogs don't follow it. A light box coming out of a dark app, right next to the product's name,
  is the clearest tell that an app was ported rather than written for Windows.
- **The copy stays in `content.rs`.** A dialog template would put it in a resource script, where the shared module and
  its tests can't reach it.

Registering with `msg_hook` is what gives the box Tab between the links and the button, Esc to close, and Enter on the
default button. Without it none of those keys do anything, because a hand-made popup isn't a dialog to Windows.

## Decision: the licence and the links are `SysLink` controls

**Decision:** the licence sentence is one `SysLink` with the licence name clickable inside it, and each site is its own.

**Why:** `SysLink` is the native hyperlink control, so it gets the system's link colour, keyboard focus, and the hand
cursor for free. It takes a tiny HTML subset, which is why `LicenseLine::markup` exists and why it escapes `&` and `<`:
a label carrying either would otherwise be read as markup. The manifest already asks for comctl32 v6, without which the
class doesn't exist at all.

## Gotcha: the app icon comes from group icon 1

`build.rs` writes the icon into the executable as `RT_GROUP_ICON` ordinal 1 (`build-support/win_resources.rs`), so
`LoadImageW` asks for that ordinal by pointer value rather than by name. Asking for it at the size the control will draw
it is what makes Windows pick the right image out of the icon instead of scaling the 256-pixel one down.

## Gotcha: the macOS window leaks its views

`macos.rs` calls `std::mem::forget` on everything at the end, so each open leaks a few KB. The deduplication guard stops
it stacking, but closing and reopening leaks again. It's the same shape as `onboarding` and `settings::window`, and
acceptable because these windows live for the app's lifetime in most usage. The Windows side doesn't have this: it
deletes its fonts on `WM_DESTROY` and clears the thread-local, so reopening is free.

## What hasn't met a Windows machine yet

`windows.rs` type-checks and lints through `./scripts/check.sh --check windows-cross`, and **has never run**. Everything
below needs a person at a Windows box:

- Whether the layout constants leave the text room at 100%, 125%, 150%, and 200% DPI. They're a fixed table scaled by
  the window's DPI, not measured from the text, so the licence line is the one most likely to want a third row.
- Whether the dark-mode ordinals do what they're documented to do here, and what the `SysLink` link colour looks like
  against the dark background. `platform/windows/dark_mode.rs` says what's uncertain.
- Whether `IsDialogMessageW` really gives the box Tab, Esc, and Enter through the shared hook. Nothing has exercised
  that seam yet on either side.
- `about_opens_without_holding_up_the_app` in `tests/e2e_shared.rs` runs there the first time the suite does, and it's
  the one that would catch a nested message loop.
