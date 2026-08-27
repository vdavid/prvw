# Onboarding

First-launch welcome window, **macOS only**. Shows a four-step checklist and closes either when a file arrives via Apple
Event (transition into the viewer) or when the user dismisses it (quit).

## Decision: Windows and Linux get no onboarding window at all

**Decision:** M6 built the About box and deliberately built no onboarding anywhere else. The launch empty state
(`app::EmptyState::NothingOpen`) is the whole of it off macOS.

**Why:** take the four steps one at a time and there's nothing left.

1. **"Install Prvw.app"** is always checked here, because running the binary means it's installed. Elsewhere the
   installer ran. Nothing to say.
2. **"Set Prvw as your default image viewer"** can't be done on Windows. Programmatic default-handler setting was
   removed in Windows 10 20H2: an app can register its ProgIDs (the installer's job) and deep-link to
   `ms-settings:defaultapps`, and that's all. Meanwhile Windows already asks "How do you want to open this file?" the
   first time someone double-clicks a JPEG after installing something new, so a step here duplicates the system's own
   flow and does it worse.
3. **"Move Prvw.app to /Applications"** is meaningless where an installer places the binary.
4. **"How to open images"** is one sentence, and the empty state is already carrying it.

So macOS keeps 2,097 lines of AppKit that exist mostly to prompt for file associations under an API only macOS has, and
the honest Windows answer is a sentence in a window that already had to exist. `docs/specs/windows-ui-design.md`,
"Onboarding and about", is the full argument.

**What this leaves for later**, so nobody reads the absence as an oversight: that spec also wanted a line on the Windows
empty state saying Prvw isn't the default viewer yet, with a "Set as default" affordance. M6 didn't build it, for two
reasons. The empty state's whole canvas is already one click target that opens the file picker (`App::window_event`), so
a second affordance inside a glyphon text pill would either steal that click or need a hit-region the overlay has no
concept of. And the capability it would reach for is `SettingKey::FileAssociations`, which the registry already tracks
and M4 owns, so building a second door to a room that isn't built yet would be a nag with nowhere to go. It belongs with
that panel.

| File                   | Purpose                                                                      |
| ---------------------- | ---------------------------------------------------------------------------- |
| `mod.rs`               | Window + widget construction, `OnboardingDelegate`, polling, close handling  |
| `checkmark.rs`         | Render the custom SVG checkmark to `NSImage` via runtime `NSBezierPath` draw |
| `defaults_sentence.rs` | Pure module: `FormatHandler` → natural-language "what currently opens what"  |

## Four-step state model

`OnboardingState` is the pure snapshot read by `OnboardingUI::render` once per poll tick. Steps:

1. **Install Prvw.app**: always checked. Running the binary means installed, so no dynamic state.
2. **Set Prvw as your default image viewer**: checked iff every UTI in `SUPPORTED_UTIS` resolves to Prvw. The "Set as
   default" button disables when checked; `defaults_sentence::describe_defaults` produces the "Current defaults" summary
   line below the button.
3. **Move Prvw.app to /Applications**: checked iff the binary path starts with `/Applications/`. Computed **once** on
   window open. The path doesn't change at runtime. An extra hint row shows only in the unchecked case.
4. **How to open images**: no checkmark. Copy switches on step 2's state: if Prvw is already default, "double-click any
   image"; else "right-click → Open with → Prvw".

## Checkmark rendering

The spec pins a specific green (`#189d34`) checkmark SVG path. We don't ship a PDF or bitmap asset. `checkmark.rs`
parses the path string at runtime, converts each command (`M`, `c`, `s`, `q`, `a`, `l`) to `NSBezierPath` operations,
and fills it into an `NSImage` via `lockFocus` / `unlockFocus`. Elliptical arcs (the `a` command) are converted to cubic
Béziers using the SVG 1.1 F.6 algorithm plus the standard `t = (4/3) * tan(δ/4)` approximation per ≤90° sub-arc.

Two variants:

- `Green`: solid `#189d34` for a completed step.
- `Dim`: macOS `labelColor` at 15% alpha. Reads as a placeholder in both light and dark appearance.

Both images are built once per window open and shared across steps 2 and 3 by `NSImageView::setImage(…)` on each poll
tick.

Why not a PDF asset? No SVG→PDF tool is installed on dev machines we care about, and pulling in an asset pipeline or
committing a pre-rendered PDF for a single glyph isn't worth the maintenance overhead. The runtime path parser is ~350
lines with tests, no external deps.

## Polling integration

Same pattern as `file_associations::settings_panel`: one `NSTimer` at 1-second intervals on the `OnboardingDelegate`. On
each tick the delegate rebuilds `OnboardingState::current()` and calls `OnboardingUI::render`. This picks up handler
changes made outside Prvw (Finder "Get Info → Open with → Change All…", other viewers' onboarding flows).

The poll also refreshes the "Current defaults" sentence, so if another app grabs JPEG while this window is up, the user
sees the change reflected without clicking anything.

## Close = quit

While the onboarding is up no winit window exists, so a raw AppKit close wouldn't propagate to winit's event loop.
`OnboardingDelegate::windowWillClose:` sends `AppCommand::Exit` so a user-dismiss cleanly terminates the process.

`close_window()` (called when a file arrives via Apple Event and we're transitioning into the viewer) detaches the
window delegate **before** calling `close` so the transition isn't misread as a user dismiss.

## Gotchas

- **Non-modal, not `runModal`.** The Apple Event delivering a file has to reach the event loop while onboarding is
  visible. A modal run loop blocks that.
- **`Retained<>` for everything.** Every AppKit widget must stay alive for the window's lifetime, or autorelease-pool
  cleanup segfaults. See the `retained_views` pattern at the end of `show_window`.
- **`load_app_icon` falls back to the folder icon in dev builds.** When launched out of `target/release/prvw` (not a
  `.app` bundle), the bundle lookup misses and we show a generic icon. Not a bug. Expected for non-packaged runs.
