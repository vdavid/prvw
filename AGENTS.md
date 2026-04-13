# Prvw

This file is for AI agents. Human contributors, see [CONTRIBUTING.md](CONTRIBUTING.md).

Prvw is a fast, minimal image viewer for macOS built with Tauri 2 (Rust backend + webview frontend). Think ACDSee 2.41:
open a pic, see it instantly, zoom/pan, arrow keys for next/prev (preloaded in background), ESC to close. Free forever
for personal use (BSL license). Website at [getprvw.com](https://getprvw.com).

- Desktop app dev: `pnpm dev` (from repo root), or `cd apps/desktop && pnpm tauri dev`
- Desktop app dev with a file: `pnpm dev -- -- /path/to/image.jpg`
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

## File structure

This is a monorepo:

- `apps/desktop/` - The desktop app (Tauri 2)
  - `src-tauri/` - Rust backend (Tauri commands, image loading, preloader, menus)
  - `src/` - Frontend (Svelte 5 + SvelteKit SPA rendered in webview)
- `apps/website/` - getprvw.com marketing website (Astro + Tailwind v4)
- `scripts/check/` - Go-based unified check runner
- `docs/` - Dev docs
  - `architecture.md` - Map of all subsystems
  - `style-guide.md` - Writing, code, and design style rules
  - `design-principles.md` - Product design values
  - `specs/` - Feature specs and plans
- Feature-level docs live in **colocated `CLAUDE.md` files** next to the code.

## Testing and checking

Always use the checker script for compilation, linting, formatting, and tests. Its output is concise and focused.

- Specific checks: `./scripts/check.sh --check <name>` (for example, `--check clippy`, `--check rustfmt`). Use
  `--help` for the full list, or multiple `--check` flags.
- All Rust checks: `./scripts/check.sh --rust`
- All Svelte checks: `./scripts/check.sh --svelte`
- All Go checks: `./scripts/check.sh --go`
- All checks: `./scripts/check.sh`
- Specific Rust tests by name: `cd apps/desktop/src-tauri && cargo test <test_name>`
- CI: Runs on PRs and pushes to main for changed files. Full run: Actions -> CI -> "Run workflow".

## Debugging

- **Logging**: Use `RUST_LOG=debug` or target specific modules like `RUST_LOG=prvw::preloader=debug`.
- **Tauri**: Use `RUST_LOG=tauri=debug` for Tauri framework diagnostics. The webview's devtools can be opened from the app for frontend debugging.

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
- ❌ **NEVER build/run the Tauri app with raw `cargo build` or `cargo run`.** It produces a binary without the embedded
  frontend (white screen). Always use `pnpm tauri dev` for development or `pnpm tauri build` for release builds. The
  `beforeDevCommand`/`beforeBuildCommand` in `tauri.conf.json` runs the Vite dev server / frontend build — skipping it
  breaks the app. `cargo check`, `cargo test`, and `cargo clippy` are fine (they don't produce runnable binaries).
- We use [mise](https://mise.jdx.dev/) to manage tool versions (Go, Node, etc.), pinned in `.mise.toml`. Rust is managed
  by `rust-toolchain.toml` at repo root.

## Gotchas

- **Use `std::thread` for CPU-bound work, not `tokio`.** The preloader does CPU-bound image decoding. `std::thread` +
  channels is the right tool. `tokio` adds unnecessary weight and complexity.
- **Tauri asset protocol for local files.** Images are served to the webview via Tauri's asset protocol
  (`asset://localhost/`), not by reading files from JS directly. The scope in `tauri.conf.json` must allow the paths.

## Workflow

- **Always read** [style-guide.md](docs/style-guide.md) before touching code. Especially sentence case!
- Cover your code with tests until you're confident. Don't go overboard.
- **Run `./scripts/check.sh` before every commit.** It takes ~12 seconds and catches formatting, linting, and test
  failures that CI will reject. Non-CI mode auto-formats; CI mode only checks. Don't skip this. Never `tail`, `head`,
  or truncate the checker output. Its output is already concise.
- **Don't commit unless explicitly asked.** Make changes, verify they work, then wait for the user to say "commit".

Happy coding! :)
