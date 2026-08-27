# Prvw

This file is for AI agents. Human contributors, see [CONTRIBUTING.md](CONTRIBUTING.md).

Prvw is a fast, minimal image viewer for macOS written in Rust (`winit` + `wgpu` + `muda`). Think ACDSee 2.41: open a
pic, see it instantly, zoom/pan, arrow keys for next/prev (preloaded in background), ESC to close. Free forever for
personal use (BSL license). Website at [getprvw.com](https://getprvw.com).

**Supported platforms.** macOS is the shipping target. Onboarding, QuickLook previews, and installing an update in place
are macOS-only. Windows has a native menu bar with working accelerators, its own About box under Help, its own Win32
settings dialog (`src/settings/windows/`), its own Win32 browse mode (`src/browser/windows/`), printing, preview
placeholders of its own (`src/previews/generator.rs`, reading the shell thumbnail cache), a startup update check that
opens the download in a browser (`src/updater/`), display-profile matching (the differentiator: images transformed into
the calibrated monitor's own ICC profile, re-read when the window crosses screens), and the cross-platform core (decode,
RAW pipeline, color transform, settings, navigation, and the header-only dimension read that sizes the window before the
first pixel paints). That is the whole parity table: 111 of 117 done, six not applicable, nothing missing. Linux has the
core, no menu bar, no settings window, no browser, and no display profile. Windows deliberately gets no onboarding
window, and the launch empty state is the whole of it there (`apps/desktop/src/onboarding/CLAUDE.md` says why). Neither
ships a release yet, and `docs/parity.md` is the honest per-item picture. Anything you write has to at least compile for
all three: check Windows and Linux from this Mac with `./scripts/check.sh --check windows-cross --check linux-cross`
(see below), and prefer a cross-platform implementation over a `#[cfg]` fence when the cost is comparable.

**Never compare paths with `==` or `Path::starts_with`.** Both are byte-wise, and on Windows the same folder arrives
spelled three ways: `canonicalize` returns `\\?\C:\...`, argv returns whatever the user typed, and a drive enumeration
uppercases. `src/paths.rs` owns the rule, holds each platform's policy as data so any host can test all three, and says
where a verbatim prefix may be stripped (display and shell APIs) and where it must not (filesystem I/O, or deep
libraries stop opening).

The main window has two top-level screens that swap: **image mode** (the wgpu viewer) and **browse mode** (a native
folder tree + thumbnail grid; `src/browser/`). Enter (in image mode) enters browse; `f`/`F11` keep toggling fullscreen
(Enter no longer does). A directory CLI argument boots into browse at that folder. macOS builds the browser out of
AppKit and Windows out of Win32 common controls; the two look deliberately different and share every model underneath.
Linux has no browser: Enter does nothing there, the menu doesn't offer it, and a directory argument opens the folder's
images in image mode instead. See `docs/specs/image-browser.md`, `src/browser/CLAUDE.md`, and
`src/browser/windows/CLAUDE.md`.

**How a file reaches the app.** A path on the command line, a Finder double-click (macOS, through an Apple Event),
**File → Open…** (Cmd+O on macOS, Ctrl+O elsewhere, every platform, `src/open_dialog.rs`), or a **drop onto the window**
(every platform, through winit's `DroppedFile`). A drop follows the same rule as the command line, in
`launch::classify_open_request`: images open as a set, a lone folder is browsed on macOS and Windows and played in image
mode on Linux (the one platform with no browser), and anything Prvw can't decode is ignored rather than opened and
failed. A launch with nothing to open waits for Finder on macOS and puts up an empty window everywhere else;
`src/launch.rs` decides, and `apps/desktop/CLAUDE.md` has the full picture.

- Desktop app: `cd apps/desktop && cargo run -- <image_path_or_dir>`
- Website dev: `cd apps/website && pnpm dev`

## Principles

These are general principles for the whole project. We live these:

1. **Instant response.** The image must appear the moment the user opens it. No loading screens, no spinners. Preload
   adjacent images so navigation feels zero-latency.
2. **Respect resources.** Minimize CPU, memory, and GPU use. Don't keep the GPU busy when idle. Use render-on-demand,
   not a continuous render loop. The thing this rules out is **idle and background cost**: Prvw is short-lived, so
   someone opens it, looks, and closes it. Within that short life, being fast wins over being frugal, and a resource
   that makes the viewer faster while it's open is one worth spending.
3. **Elegant simplicity.** This is a viewer, not an editor. Every feature must earn its place. Prefer doing fewer things
   exceptionally well over doing many things adequately.
4. **Rock-solid feel.** The UI must always be responsive. Never block the main thread. Handle edge cases (corrupt
   images, huge files, missing files) gracefully.
5. **Platform-native.** The app should feel like it was made specifically for macOS. Use native menus, respect system
   settings (dark mode, accessibility). Cross-platform later, but never at the cost of native feel.

### Technical principles

1. **Think from first principles, capture intention.** Add logs. Run the code. Do benchmarks. Then document the "why"s
   and link the data where needed.
2. **Invest in finding the right tradeoff.** Elegance lives between duplication and overengineering. No premature
   abstractions, but no copy-paste either.
3. **Invest in tooling.** We have check runners, linters, CI. Tooling must be fast so we use it, and strict so it
   doesn't allow us to make mistakes.
4. **Name internals after the UI.** When a feature or action has a user-facing name, its internal identifiers (command
   ids, file/function/type names, settings keys, menu handlers) use the same vocabulary. If the View menu says "Sort by
   date", the code says `sort_by_date`, not `order_by_mtime`. A mismatch forces every reader to keep a mental
   translation table, and it rots as the label drifts. Rename internals when you rename the UI.

## File structure

This is a monorepo:

- `apps/desktop/` - The Rust desktop app (`winit` + `wgpu` + `muda`)
  - `installer/windows/` - The NSIS installer, built by `scripts/build-windows-installer.sh` (from macOS on demand, on
    `windows-latest` at release)
- `apps/website/` - getprvw.com marketing website (Astro + Tailwind v4)
- `xtask/` - Dependency-free repo tasks that read the app's registries without building it (`cargo xtask parity`)
- `scripts/check/` - Go-based unified check runner
- `docs/` - Dev docs
  - `architecture.md` - Map of all subsystems
  - `parity.md` - Generated: what each platform's UI owes the app and what it has. Never edit by hand
  - `style-guide.md` - Writing, code, and design style rules
  - `design-principles.md` - Product design values
  - `mcp-server.md` - MCP/QA server tool and resource reference
  - `specs/` - Feature specs and plans
- Feature-level docs live in **colocated `CLAUDE.md` files** next to the code.

## Testing and checking

Always use the checker script for compilation, linting, formatting, and tests. Its output is concise and focused.

- Specific checks: `./scripts/check.sh --check <name>` (for example, `--check clippy`, `--check rustfmt`). Use `--help`
  for the full list, or multiple `--check` flags.
- All Rust checks: `./scripts/check.sh --rust` (workspace-wide: `apps/desktop` and `xtask`)
- All Go checks: `./scripts/check.sh --go`
- All checks: `./scripts/check.sh`
- Specific Rust tests by name: `cd apps/desktop && cargo test <test_name>`
- **E2E tests** spawn the real binary and drive it through the QA HTTP server. `tests/e2e_shared.rs` runs on every
  platform, `tests/e2e_macos.rs` holds what has to poke a native widget, and `tests/e2e/` is the harness. A shared test
  names the actions it exercises and the parity registries decide whether the host runs it: see `src/qa/CLAUDE.md`. The
  harness pipes the app's stderr, so a request that fails panics with the app's own log, whether the process is still
  alive, and its exit status in hex — which is the only account a Windows crash leaves behind.
- **`PRVW_TEST_NO_FAIL_FAST=1`** makes the `cargo-test` check pass `--no-fail-fast` to nextest, which otherwise cancels
  the whole run on the first test failure. CI's Windows leg sets it, because that's the platform with the least evidence
  behind it and one cancelled run there leaves hundreds of tests unexecuted; macOS and Linux keep fail-fast, which saves
  real minutes on a green history. Set it locally for a run where you want the full picture.
- On Windows, `scripts/check.ps1` replaces `check.sh` (which is bash). Same flags, same exit code, same Go runner.
- CI: Runs on PRs and pushes to main for changed files. A Rust change runs clippy and the tests on Linux, macOS, and
  Windows. Full run: Actions -> CI -> "Run workflow".

### Checking the Windows and Linux builds from macOS

`./scripts/check.sh --check windows-cross` type-checks and lints the desktop app for `x86_64-pc-windows-msvc` without a
Windows machine, so Windows-only code gets a real feedback loop. It's marked slow, so a plain `./scripts/check.sh`
leaves it out.

Setup:

- `cargo install cargo-xwin --locked`
- `rustup target add x86_64-pc-windows-msvc`
- `rustup component add llvm-tools`

The `rustup` lines are per-toolchain, and the `cargo install` is genuinely once. `rust-toolchain.toml` pins an exact
version, so a Renovate bump lands a fresh toolchain carrying neither the target nor `llvm-tools`, and the check names
what's missing when you run it.

That file lists `components = ["clippy", "rustfmt"]` but deliberately no `targets`, which is the split to keep in mind
when editing it. clippy and rustfmt are 4 MB and every CI job runs both, so they belong in the file and CI breaks
without them. The cross-check targets are 76 MB with `llvm-tools`, and no CI job opens them: the Windows and Linux legs
compile natively, and the cross-checks only ever run on a developer's Mac. Putting them in `[toolchain]` would tax all
five jobs on every run to spare one machine the two commands above.

The first run downloads the MSVC CRT and Windows SDK headers into `~/Library/Caches/cargo-xwin/`, which takes about a
minute; later runs are incremental and finish in seconds. The check links rustup's `llvm-ar` into
`target/cross-check-bin/llvm-lib` on its own, because cargo-xwin ships clang-cl and lld-link but no MSVC archiver.
`aarch64-pc-windows-msvc` works with the same recipe once you add that target.

The check stops at compiling, so it catches `cfg` and API-shape mistakes and says nothing about runtime behavior. For a
binary you can actually run, the same toolchain links one:

```bash
PATH="$(git rev-parse --show-toplevel)/target/cross-check-bin:$PATH" \
  cargo xwin build --target x86_64-pc-windows-msvc -p prvw
```

That writes `target/x86_64-pc-windows-msvc/debug/prvw.exe`, a real PE32+ binary to copy into a Windows VM. Run the
`windows-cross` check at least once first, so the `llvm-lib` link exists.

It's a GUI-subsystem binary carrying its icon, application manifest, and version info, all built by `build.rs` with no
resource compiler involved (see `apps/desktop/CLAUDE.md`). Read the PE back to check any of that from the Mac, with the
`llvm-readobj` in `$(rustc --print sysroot)/lib/rustlib/<host-triple>/bin/` (the `llvm-tools` component):

```bash
llvm-readobj --file-headers --coff-resources target/x86_64-pc-windows-msvc/debug/prvw.exe
```

`Subsystem: IMAGE_SUBSYSTEM_WINDOWS_GUI` and 10 resources under `ICON`, `GROUP_ICON`, `VERSIONINFO`, and `MANIFEST` are
what a good build looks like.

`./scripts/check.sh --check linux-cross` is the Linux twin: the same clippy run against `x86_64-unknown-linux-gnu`, also
slow-marked. Setup is `rustup target add x86_64-unknown-linux-gnu` (again, per-toolchain) and `mise install zig@latest`.
zig supplies the Linux C toolchain that `zstd-sys` needs, since Apple's command line tools cross-compile to nothing. The
check writes its own `cc` and `ar` wrappers into `target/cross-check-bin/`; they exist because cc-rs passes the Rust
triple as `--target=x86_64-unknown-linux-gnu`, which zig can't parse.

zig is deliberately **not** in `.mise.toml`: pinning it would make all five CI jobs download it for a check that only
runs on a developer's Mac. The check finds it through `mise where zig` instead.

## Debugging

- **Logging**: Use `RUST_LOG=debug` or target specific modules with `RUST_LOG=prvw::render::renderer=debug`. On Windows
  the app has no console of its own, so a launch from Explorer logs to `%APPDATA%\Prvw\prvw.log` instead;
  `apps/desktop/CLAUDE.md` has the full order of preference.
- **GPU issues**: Prvw logs the adapter it chose, its device type, the backend, and the driver at `info` on startup
  (`render::renderer`), which is the line a QA report from unfamiliar hardware should carry. wgpu keeps its own account
  of the same decision, including every adapter it considered and rejected, at `debug`: `RUST_LOG=wgpu_core=debug`.
  `render/gpu.rs` decides what it's asked for in the first place.

## Code intelligence

This repo carries a CodeGraph index (`.codegraph/`, gitignored). How to use the tools is covered at user level; the
prvw-specific point: unlike a Tauri app, prvw has no IPC barrier, so the call graph (`codegraph_callers` /
`codegraph_callees` / `codegraph_impact`) resolves direct Rust→Rust edges with few blind spots — trustworthy enough to
lean on for "what breaks if I change this". The edges it still can't see are macros, trait-object dynamic dispatch, and
the `objc2`/AppKit `msg_send!` boundary (ObjC selectors are string-dispatched, so menu/responder wiring is invisible).
Verify a "no callers" against those before treating a symbol as dead.

## Where to put instructions

- **User-generic preferences** (for example, "never use git stash") -> `~/.claude/CLAUDE.md`. These apply across all
  projects.
- **Project-specific instructions** -> `AGENTS.md` (this file) for repo-wide rules, or colocated `CLAUDE.md` files for
  module-specific docs. These are version-controlled and visible to all contributors.

## Critical rules

- ❌ NEVER use `git stash`, `git checkout`, `git reset`, or any git write operation unless explicitly asked. Multiple
  agents may be working simultaneously.
- ❌ NEVER add dependencies without checking license compatibility and verifying the latest version from crates.io/npm.
  Never trust training data for versions.
- ❌ Don't ignore linter warnings. Fix them or justify with a comment.
- We use [mise](https://mise.jdx.dev/) to manage tool versions (Go, Node, etc.), pinned in `.mise.toml`. Rust is managed
  by `rust-toolchain.toml` at repo root.

## Gotchas

- **wgpu surface must be created in `resumed()`, not at startup.** `winit` 0.30 uses the `ApplicationHandler` trait. The
  window and `wgpu` surface must be created inside `resumed()`, which fires after the event loop starts. Creating them
  earlier crashes on macOS.
- **Use `std::thread` for CPU-bound work, not `tokio`.** The preloader does CPU-bound image decoding. `std::thread` +
  channels is the right tool. `tokio` adds unnecessary weight and event-loop integration complexity with `winit`.
- **Keep objc2 `Retained<>` wrappers alive during AppKit modal sessions.** When creating NSTextField, NSButton, or other
  views via objc2 and running a modal window (`runModalForWindow`), store all `Retained<>` objects in a Vec that lives
  for the modal's duration. Dropping them early causes segfault in autorelease pool cleanup. No compile-time check
  exists for this. See `apps/desktop/CLAUDE.md` for details.
- **Never run AppKit modals inside winit's event loop.** `runModalForWindow` inside `resumed()` or any winit callback
  creates a nested run loop that segfaults on autorelease pool cleanup. Run native modals BEFORE `EventLoop::new()`
  instead (see onboarding in `main()`).
- **Never open a nested message loop on Windows either.** Same rule, different failure: a Win32 modal loop
  (`DialogBoxParam`, `TaskDialogIndirect`, `IFileDialog::Show`) doesn't crash, it starves winit's pump, so
  `about_to_wait` stops running and the slideshow's timer freezes. Modeless (`CreateDialogParamW`) plus the one message
  hook in `platform::windows::msg_hook` is the shape that works; read that module before adding anything to the pump.

## Worktrees

Solo workflow: branch off **local** `main`, work under `.claude/worktrees/`. To land: rebase onto current local `main`
(it usually has advanced — unpushed commits land there directly), fast-forward `main` to the branch, then delete the
worktree + branch. One gitignored bit is worth copying in (git won't carry it):

- **`target/` (optional, big speedup):** `cp -Rc target <worktree>/target` from the repo root (the workspace target is
  at the root, not per-app). Deps are fingerprinted on version/features/rustc/profile, so only workspace members
  rebuild.

The E2E test windows open unfocused and behind everything (the harness sets `PRVW_BACKGROUND_WINDOW`), so a run won't
grab your keystrokes. See `window::background_window_requested`.

## Workflow

- **Always read** [style-guide.md](docs/style-guide.md) before touching code. Especially sentence case!
- Cover your code with tests until you're confident. Don't go overboard.
- **Run `./scripts/check.sh` before every commit.** It takes ~10 seconds (16 checks across Rust, Go, and Astro) and
  catches formatting, linting, and test failures that CI will reject. Run all checks, not just `--rust`. Non-CI mode
  auto-formats; CI mode only checks. Don't skip this. Never `tail`, `head`, or truncate the checker output. Its output
  is already concise.
- **Commit at will once work is verified.** When a change is done and the checks pass, commit it without waiting to be
  asked. Group related changes into focused commits with good messages (see `.claude/rules/git-conventions.md`). Don't
  push, though: pushing stays gated on an explicit request (see the `push-cadence` user rule).

Happy coding! :)
