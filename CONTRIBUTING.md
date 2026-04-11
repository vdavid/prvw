# Contributing to Prvw

Welcome! Here's how to get around.

## Prerequisites

- [Rust](https://rustup.rs/) (stable, managed by `rust-toolchain.toml` at repo root)
- [mise](https://mise.jdx.dev/) for Go, Node, and pnpm versions (pinned in `.mise.toml`)
- macOS (the app uses Metal for GPU rendering)

Run `mise install` once after cloning to set up the right tool versions.

## Running the desktop app

Prvw needs an image file path as an argument:

```bash
cd apps/desktop
cargo run -- ~/Pictures/photo.jpg
```

For a quick test with any image on your system:

```bash
cd apps/desktop
cargo run -- "$(find ~/Pictures -name '*.jpg' -o -name '*.png' | head -1)"
```

Useful env vars:

- `RUST_LOG=debug` for verbose logging
- `RUST_LOG=prvw::renderer=debug` for GPU-specific logs
- `RUST_LOG=prvw::preloader=debug` for preloading logs

## Running the website

```bash
pnpm install          # once, from repo root
pnpm dev:website      # starts Astro dev server on port 14829
```

Or build it:

```bash
pnpm build:website
```

## Running checks

The check runner catches formatting, linting, and test issues before you push:

```bash
./scripts/check.sh              # all 14 checks
./scripts/check.sh --rust       # Rust checks only (rustfmt, clippy, cargo-test)
./scripts/check.sh --go         # Go checks only (scripts)
./scripts/check.sh --check clippy  # one specific check
./scripts/check.sh --help       # full list
```

## Running tests

```bash
# All Rust tests
cd apps/desktop && cargo test

# A specific test
cd apps/desktop && cargo test view::tests::zoom_clamped

# All checks (includes tests)
./scripts/check.sh
```

## Building a release

```bash
# Build + sign (requires the Developer ID certificate)
./scripts/build-and-sign.sh

# Build without signing
cd apps/desktop && cargo build --release
```

The release binary lands in `target/release/prvw`.

## Project structure

```
apps/desktop/     Rust desktop app (winit + wgpu + muda)
apps/website/     getprvw.com (Astro + Tailwind v4)
scripts/check/    Go check runner (14 checks)
docs/             Dev docs (architecture, style guide, design principles)
```

Each directory with non-obvious patterns has a `CLAUDE.md` file explaining the architecture.

## Style rules

Read [docs/style-guide.md](docs/style-guide.md) before writing code or copy. The short version:

- **Sentence case** in all headings, labels, and titles
- **Active voice**, contractions, casual tone
- **No em dashes** (use commas, parentheses, or new sentences)
- **Rust**: 120 char lines, 4-space indent

## Reporting issues

Use the [issue tracker](https://github.com/vdavid/prvw/issues). Include your macOS version and any relevant logs
(`RUST_LOG=debug`).
