# Check runner

Go CLI that runs all code quality checks for the Prvw monorepo in parallel with dependency ordering. Ported from
[Cmdr's check runner](https://github.com/vdavid/cmdr/tree/main/scripts/check). Invoked via `./scripts/check.sh`.

## Quick start

```bash
./scripts/check.sh                    # All checks
./scripts/check.sh --app desktop      # Desktop (Rust) only
./scripts/check.sh --check clippy     # Specific check
./scripts/check.sh --rust             # All Rust checks
./scripts/check.sh --go               # All Go checks
./scripts/check.sh --ci --fail-fast   # CI mode
```

On Windows the entry point is `scripts\check.ps1`, which takes the same flags and returns the same exit code.

## Architecture

```
./scripts/check.sh [flags]
  -> go run ./scripts/check [flags]
    -> ValidateCheckNames()          # startup: catch ID/nickname collisions
    -> parseFlags()
    -> findRootDir()                 # walk up to AGENTS.md
    -> selectChecks()                # filter AllChecks by flags
    -> FilterSlowChecks()
    -> ensurePnpmDependencies()      # pnpm install once at root (skipped for non-website runs)
    -> Runner.Run():
        goroutine pool (NumCPU semaphore)
        dependency graph: canStart() checks DependsOn
        status line goroutine (200ms tick, TTY only)
    -> print summary, exit 0/1
```

## Key files

| File                          | Purpose                                                                             |
| ----------------------------- | ----------------------------------------------------------------------------------- |
| `main.go`                     | Entry point: flags, root dir, check selection, runner delegation                    |
| `runner.go`                   | Parallel executor: goroutine pool, dependency graph, TTY status line                |
| `checks/common.go`            | Core types, shared utils (`RunCommand`, `EnsureGoTool`, `runESLintCheck`)           |
| `checks/common_unix.go`       | Process-group setup and tree kill on macOS and Linux                                |
| `checks/common_windows.go`    | The same, via job objects, on Windows                                               |
| `checks/walk.go`              | `findFiles` / `countFiles`: the file counting every check does                      |
| `console_{windows,other}.go`  | `prepareConsole()`: UTF-8 and ANSI on the Windows console                           |
| `checks/registry.go`          | `AllChecks`: canonical ordered list, lookup and validation functions                |
| `checks/desktop-rust-*.go`    | Rust checks (rustfmt, clippy, cargo-test, parity, windows-cross, linux-cross)       |
| `checks/windows-installer.go` | The Windows installer's generated registry include, plus two facts about `prvw.nsi` |
| `checks/oxfmt.go`             | Monorepo-wide formatter (oxfmt, prettier-compatible)                                |
| `checks/website-*.go`         | Website checks (eslint, typecheck, build)                                           |
| `checks/scripts-go-*.go`      | Go checks (gofmt, go-vet, staticcheck, misspell, gocyclo, deadcode, tests)          |
| `checks/conflict-markers.go`  | Unresolved merge conflict markers in source and docs                                |
| `stats.go`                    | CSV stats logging (`~/prvw-check-log.csv`)                                          |
| `colors.go`                   | ANSI color constants                                                                |
| `utils.go`                    | `findRootDir()` (walks up until `AGENTS.md` is found)                               |

## Adding a new check

1. Create `checks/{app}-{name}.go` with `func RunSomething(ctx *CheckContext) (CheckResult, error)`.
2. Register in `AllChecks` in `registry.go`.
3. Return `Success("message")` on pass, `fmt.Errorf(...)` on fail, `Skipped("reason")` to skip.
4. Run `./scripts/check.sh --go` to verify, then break the thing on purpose and watch it fail for the right reason.

`checks/windows-installer.go` is the most recent one, and `checks/conflict-markers.go` the one before it.

## Key patterns

- **Graceful skipping**: Rust and website checks skip if their directory/`Cargo.toml` doesn't exist yet.
- **Auto-fix vs CI**: `--ci` disables auto-fixing. Formatters fix locally, report-only in CI.
- **IDs vs nicknames**: `--check` accepts either. `CLIName()` returns nickname if set, else ID.
- **CSV stats**: Each run appends to `~/prvw-check-log.csv`. Disabled by `--no-log` or `--ci`.

## Apps and checks

| App     | Tech      | Checks                                                                                          |
| ------- | --------- | ----------------------------------------------------------------------------------------------- |
| Other   | 📐 Format | oxfmt (monorepo-wide; runs first, gates eslint)                                                 |
| Desktop | Rust      | rustfmt, clippy, cargo-test, parity, installer, windows-cross + linux-cross (both slow, opt-in) |
| Website | Astro     | eslint, typecheck, build                                                                        |
| Scripts | Go        | gofmt, go-vet, staticcheck, misspell, gocyclo, deadcode, tests                                  |
| Other   | -         | changelog-commit-links, conflict-markers                                                        |

## The conflict-marker check

`conflict-markers` (nickname `conflicts`) fails when `<<<<<<< `, `||||||| `, or `>>>>>>> ` starts a line in any source
or docs file, including the repo root's own markdown.

It exists because one got through. A rebase resolution was staged with `git add -A` while `AGENTS.md` still carried its
markers; oxfmt then reflowed `>>>>>>>` into a nested blockquote, which is what a Markdown formatter thinks that is, and
the result read as prose in every later diff. Nothing else in this runner reads English, so nothing else would have
caught it.

- **`=======` is deliberately not matched.** It's a legal Markdown setext heading underline, and matching it would fail
  on ordinary documents. The other three markers have no legitimate use at column zero.
- **Column zero is the rule**, so a doc can discuss a marker by indenting it or putting it inline. This section relies
  on that.

## Cross-platform notes

The runner builds and runs on macOS, Linux, and Windows. Three things carry that, and each has a trap:

- **Killing a check kills its whole tree.** `cargo` spawns `rustc` children, and a wedged child that outlives the runner
  is the failure mode this guards against. Unix does it with `Setpgid` plus a signal to the negative PID; Windows does
  it with a job object per child. Both hide behind the same four functions (`prepareProcessGroup`, `trackProcessGroup`,
  `killProcessGroup`, `releaseProcessGroup`), so `RunCommand` stays platform-blind. ❌ Don't reach for `syscall` in a
  check: add to the per-OS pair instead.
- **Counting files goes through `findFiles`, never `find`.** On Windows `find` resolves to
  `C:\Windows\System32\find.exe`, a text search tool that returns plausible-looking garbage rather than failing.
  `findFiles` matches `find <dir> -type f -name ...` exactly: symlinks excluded, hidden entries included, nothing
  pruned.
- **`go run .`, not `go run *.go`.** The explicit file list breaks the moment a file carries a build tag, which several
  now do. Both `check.sh` and `check.ps1` use `go run .`.

**Gotcha**: `EnsureGoTool` returns a path with no `.exe`. That is fine, because `os/exec` appends Windows extensions
when it starts a command given a full path.

## The parity check

`desktop-rust-parity-table` (nickname `parity`) regenerates `docs/parity.md` from the registries in
`apps/desktop/src/parity/` and compares it with the committed file: it rewrites it locally and fails in CI, the way the
formatters do. The generator is `cargo run -p xtask -- parity`, which needs no display and no GPU. Three things about it
are load-bearing:

- **It reads stdout, not the merged output.** `RunCommandSplit` keeps the streams apart, because anything cargo writes
  to stderr would land in the middle of the document. `RunCommand` still merges, for every check that reports rather
  than parses.
- **It builds in its own `CARGO_TARGET_DIR` (`target/xtask`).** Checks run in parallel, and a shared target directory
  would make this one wait behind clippy's build lock for a task that takes 300 ms.
- **`docs/parity.md` is in `.oxfmtrc.json`'s `ignorePatterns`.** Two formatters over one file means an endless
  regenerate-reformat loop, since oxfmt runs with `proseWrap: "always"`.

The Rust checks all run workspace-wide (`cargo fmt --all`, `cargo clippy --workspace`, `cargo nextest run --workspace`),
so the `xtask` crate is linted, formatted, cross-checked, and tested by the same runs as the app.

## The Windows installer check

`desktop-windows-installer` (nickname `installer`) is the parity check's twin over
`apps/desktop/installer/windows/file-associations.nsh`: `cargo xtask installer-registry` renders it from
`settings::windows::file_types`, and the check rewrites it locally and fails in CI. That's what makes "the installer and
the Settings button register the same keys" a fact rather than a claim.

It also asserts two things about the hand-written `prvw.nsi`, both of which fail silently otherwise:

- **It's UTF-8 with a BOM.** Without one, makensis reads the script in the build host's ANSI code page and the publisher
  name in the installer's version info comes out mangled, with no error anywhere.
- **It spells out no version.** The version reaches makensis as `-DPRVW_VERSION`, read from `apps/desktop/Cargo.toml`,
  which is the same field the exe's own version info comes from. A literal in the script would be a second copy that
  drifts at the next release.

It depends on `parity` only because both generators share `target/xtask` and cargo locks it. It doesn't run makensis:
that packages a 36 MB executable, and NSIS has no syntax-only mode. `./scripts/build-windows-installer.sh` is what
proves the script compiles.
