# Prvw

This file is for AI agents. Human contributors, see [CONTRIBUTING.md](CONTRIBUTING.md).

Prvw is a fast, minimal image viewer for macOS written in Rust (`winit` + `wgpu` + `muda`). Think ACDSee 2.41: open a
pic, see it instantly, zoom/pan, arrow keys for next/prev (preloaded in background), ESC to close. Free forever for
personal use (BSL license). Website at [getprvw.com](https://getprvw.com).

**Supported platforms.** macOS is the shipping target and the only one with the full feature set: native menus, the
browse-mode AppKit UI, display-profile matching, QuickLook previews, and the updater are all macOS-only. Windows and
Linux builds compile and run, and the cross-platform core (decode, RAW pipeline, color transform, settings, navigation)
is real there, but no release ships for them yet. Anything you write has to at least compile for all three: check
Windows from this Mac with `./scripts/check.sh --check windows-cross` (see below), and prefer a cross-platform
implementation over a `#[cfg]` fence when the cost is comparable.

The main window has two top-level screens that swap: **image mode** (the wgpu viewer) and **browse mode** (a macOS-only
native AppKit folder tree + thumbnail grid; `src/browser/`). Enter (in image mode) enters browse; `f`/`F11` keep
toggling fullscreen (Enter no longer does). A directory CLI argument boots into browse at that folder. See
`docs/specs/image-browser.md` and `src/browser/CLAUDE.md`.

- Desktop app: `cd apps/desktop && cargo run -- <image_path_or_dir>`
- Website dev: `cd apps/website && pnpm dev`

## Principles

These are general principles for the whole project. We live these:

1. **Instant response.** The image must appear the moment the user opens it. No loading screens, no spinners. Preload
   adjacent images so navigation feels zero-latency.
2. **Respect resources.** Minimize CPU, memory, and GPU use. Don't keep the GPU busy when idle. Use render-on-demand,
   not a continuous render loop.
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
- `apps/website/` - getprvw.com marketing website (Astro + Tailwind v4)
- `scripts/check/` - Go-based unified check runner
- `docs/` - Dev docs
  - `architecture.md` - Map of all subsystems
  - `style-guide.md` - Writing, code, and design style rules
  - `design-principles.md` - Product design values
  - `mcp-server.md` - MCP/QA server tool and resource reference
  - `specs/` - Feature specs and plans
- Feature-level docs live in **colocated `CLAUDE.md` files** next to the code.

## Testing and checking

Always use the checker script for compilation, linting, formatting, and tests. Its output is concise and focused.

- Specific checks: `./scripts/check.sh --check <name>` (for example, `--check clippy`, `--check rustfmt`). Use `--help`
  for the full list, or multiple `--check` flags.
- All Rust checks: `./scripts/check.sh --rust`
- All Go checks: `./scripts/check.sh --go`
- All checks: `./scripts/check.sh`
- Specific Rust tests by name: `cd apps/desktop && cargo test <test_name>`
- On Windows, `scripts/check.ps1` replaces `check.sh` (which is bash). Same flags, same exit code, same Go runner.
- CI: Runs on PRs and pushes to main for changed files. A Rust change runs clippy and the tests on Linux, macOS, and
  Windows. Full run: Actions -> CI -> "Run workflow".

### Checking the Windows build from macOS

`./scripts/check.sh --check windows-cross` type-checks and lints the desktop app for `x86_64-pc-windows-msvc` without a
Windows machine, so Windows-only code gets a real feedback loop. It's marked slow, so a plain `./scripts/check.sh`
leaves it out.

One-time setup:

- `cargo install cargo-xwin --locked`
- `rustup target add x86_64-pc-windows-msvc`
- `rustup component add llvm-tools`

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

There's no Linux equivalent yet; `docs/specs/cross-platform-plan.md` records what blocks it.

## Debugging

- **Logging**: Use `RUST_LOG=debug` or target specific modules with `RUST_LOG=prvw::render::renderer=debug`.
- **GPU issues**: `wgpu` logs adapter/device info at `info` level. Check `RUST_LOG=wgpu=info` for GPU backend details.

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

## Worktrees

Solo workflow: branch off **local** `main`, work under `.claude/worktrees/`. To land: rebase onto current local `main`
(it usually has advanced — unpushed commits land there directly), fast-forward `main` to the branch, then delete the
worktree + branch. One gitignored bit is worth copying in (git won't carry it):

- **`target/` (optional, big speedup):** `cp -Rc target <worktree>/target` from the repo root (the workspace target is
  at the root, not per-app). Deps are fingerprinted on version/features/rustc/profile, so only workspace members
  rebuild.

The integration-test windows open unfocused and behind everything (the harness sets `PRVW_BACKGROUND_WINDOW`), so a run
won't grab your keystrokes. See `window::background_window_requested`.

## Workflow

- **Always read** [style-guide.md](docs/style-guide.md) before touching code. Especially sentence case!
- Cover your code with tests until you're confident. Don't go overboard.
- **Run `./scripts/check.sh` before every commit.** It takes ~10 seconds (14 checks across Rust, Go, and Astro) and
  catches formatting, linting, and test failures that CI will reject. Run all checks, not just `--rust`. Non-CI mode
  auto-formats; CI mode only checks. Don't skip this. Never `tail`, `head`, or truncate the checker output. Its output
  is already concise.
- **Commit at will once work is verified.** When a change is done and the checks pass, commit it without waiting to be
  asked. Group related changes into focused commits with good messages (see `.claude/rules/git-conventions.md`). Don't
  push, though: pushing stays gated on an explicit request (see the `push-cadence` user rule).

Happy coding! :)
