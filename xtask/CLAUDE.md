# xtask

Repo tasks that read the desktop app's source of truth without building the app. Each prints a generated file to stdout;
a check in `scripts/check/` writes it and fails on a stale one.

- `cargo xtask parity` prints `docs/parity.md` (alias in `.cargo/config.toml`); `./scripts/check.sh --check parity`
  writes it.
- `cargo xtask installer-registry` prints `apps/desktop/installer/windows/file-associations.nsh`, the NSIS include the
  Windows installer compiles; `./scripts/check.sh --check installer` writes it.

| File                        | Purpose                                                                                   |
| --------------------------- | ----------------------------------------------------------------------------------------- |
| `src/main.rs`               | Task dispatch, and the `#[path]` loads of the app's parity, decoder, and file-type source |
| `src/parity_table.rs`       | Renders the registries as the Markdown of `docs/parity.md`, plus tests                    |
| `src/installer_registry.rs` | Renders `file_types::registration` and `removal` as NSIS macros, plus tests               |

## Decision: a crate of its own, with no dependencies

**Why:** the generator has to run on a headless Windows or Linux CI runner, in a second, for a task that prints text. A
`--print-parity` flag on the app, or a second `[[bin]]` in `apps/desktop`, would drag wgpu, rawler, and a GPU-shaped
dependency graph into a documentation check, because cargo builds a package's whole dependency closure for any of its
targets. This crate has no dependencies at all, so `cargo run -p xtask` is well under a second from cold and can't open
a window.

The registries come in through `#[path = "../../apps/desktop/src/parity/mod.rs"]`, the same way
`apps/desktop/tests/parity_fixtures/` loads them. That works because the parity tree is `core` and `std` only and
carries no `#[cfg]`, so one host answers for every platform. The cost is that the tree's own unit tests compile and run
twice, once per crate. That's a few milliseconds, and it buys a generator that never waits on the app.

`settings/windows/file_types.rs` comes in the same way, and it asks for its extension list through
`crate::decoding::supported_extensions`. A four-line `decoding` module in `main.rs` stands in for the app's, forwarding
to `decoding/dispatch.rs`, which holds the three extension tables and has no dependencies either.

**Gotcha:** `#[path]` on an _inline_ module resolves against a directory named after that module, so
`mod decoding { #[path = "../../apps/…"] mod dispatch; }` looks in `xtask/src/decoding/`. Anything reaching into the app
has to be declared at the crate root, which is why `dispatch` sits beside `parity` rather than inside the shim.

## Decision: the check owns the file, this crate only prints

**Why:** one writer. `scripts/check/checks/parity-table.go` compares stdout against the committed `docs/parity.md`,
rewrites it locally, and fails in CI, which is the same shape the formatters have. A `--write` flag here would be a
second writer with its own idea of where the repo root is.

## Gotcha: `docs/parity.md` is exempt from oxfmt

**Why:** oxfmt runs with `proseWrap: "always"`, so it re-wraps the generated long lines and the parity check then
regenerates them, forever. `.oxfmtrc.json` lists the file under `ignorePatterns`. Keep it there, and keep the generated
lines as long as they need to be.

## Adding a task

Add a match arm in `src/main.rs`, a module beside `parity_table.rs`, and a line in `usage()`. Then add the check that
owns the output file (`scripts/check/CLAUDE.md`, "Adding a new check"), because a generator nothing compares against is
a generator that goes stale. The workspace-wide `cargo fmt --all`, `cargo clippy --workspace`, and
`cargo nextest run --workspace` in `scripts/check/` already cover this crate.
