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

| File                                   | Purpose                                                                              |
| -------------------------------------- | ------------------------------------------------------------------------------------ |
| `main.go`                              | Entry point: flags, root dir, check selection, runner delegation                     |
| `runner.go`                            | Parallel executor: goroutine pool, dependency graph, TTY status line                 |
| `checks/common.go`                     | Core types, shared utils (`RunCommand`, `runESLintCheck`)                            |
| `checks/common_unix.go`                | Process-group setup and tree kill on macOS and Linux                                 |
| `checks/common_windows.go`             | The same, via job objects, on Windows                                                |
| `checks/walk.go`                       | `findFiles` / `countFiles`: the file counting every check does                       |
| `console_{windows,other}.go`           | `prepareConsole()`: UTF-8 and ANSI on the Windows console                            |
| `checks/registry.go`                   | `AllChecks`: canonical ordered list, lookup and validation functions                 |
| `checks/go-tools.go`                   | `EnsureGoTool`: version-pinned Go analyzers, installed into `target/check-go-tools`  |
| `checks/desktop-rust-*.go`             | Rust checks (rustfmt, clippy, cargo-test, cargo-audit, cargo-deny, parity, cross)    |
| `checks/desktop-macos-availability.go` | ObjC selector/class use vs. the `LSMinimumSystemVersion` floor                       |
| `checks/windows-installer.go`          | The Windows installer's generated registry include, plus two facts about `prvw.nsi`  |
| `checks/oxfmt.go`                      | Monorepo-wide formatter (oxfmt, prettier-compatible)                                 |
| `checks/website-*.go`                  | Website checks (eslint, typecheck, build)                                            |
| `checks/scripts-go-*.go`               | Go checks (gofmt, vet, staticcheck, misspell, gocyclo, deadcode, govulncheck, tests) |
| `checks/conflict-markers.go`           | Unresolved merge conflict markers in source and docs                                 |
| `stats.go`                             | CSV stats logging (`~/.local/share/check-runner/prvw/check-log.csv`)                 |
| `colors.go`                            | ANSI color constants                                                                 |
| `utils.go`                             | `findRootDir()` (walks up until `AGENTS.md` is found)                                |

## Adding a new check

1. Create `checks/{app}-{name}.go` with `func RunSomething(ctx *CheckContext) (CheckResult, error)`.
2. Register in `AllChecks` in `registry.go`.
3. Return `Success("message")` on pass, `fmt.Errorf(...)` on fail, `Skipped("reason")` to skip.
4. Run `./scripts/check.sh --go` to verify, then break the thing on purpose and watch it fail for the right reason.

`checks/desktop-rust-cargo-deny.go` is the most recent one, and `checks/windows-installer.go` the one before it.

## Key patterns

- **Graceful skipping**: Rust and website checks skip if their directory/`Cargo.toml` doesn't exist yet.
- **Auto-fix vs CI**: `--ci` disables auto-fixing. Formatters fix locally, report-only in CI.
- **IDs vs nicknames**: `--check` accepts either. `CLIName()` returns nickname if set, else ID.
- **CSV stats**: Each run appends to `~/.local/share/check-runner/prvw/check-log.csv`, outside the repo so a worktree
  teardown can't take the history. Disabled by `--no-log` or `--ci`.

## Apps and checks

| App     | Tech      | Checks                                                                                                                                       |
| ------- | --------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| Other   | 📐 Format | oxfmt (monorepo-wide; runs first, gates eslint)                                                                                              |
| Desktop | Rust      | rustfmt, clippy, cargo-test, cargo-audit, cargo-deny, parity, installer, macos-availability, windows-cross + linux-cross (both slow, opt-in) |
| Website | Astro     | eslint, typecheck, build                                                                                                                     |
| Scripts | Go        | gofmt, go-vet, staticcheck, misspell, gocyclo, deadcode, govulncheck, tests                                                                  |
| Other   | -         | changelog-commit-links, conflict-markers                                                                                                     |

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

## Pinned tools and advisory checks

Every tool the runner installs is pinned to an exact version, never `@latest`, for the same reason every dependency
waits three days: an install runs code on every developer machine and CI runner. `docs/security.md` has the wider
picture.

- **Go analyzers** go through `EnsureGoTool(rootDir, name, "module/cmd/x@vX.Y.Z")`, which refuses an unpinned path,
  installs into the repo's `target/check-go-tools`, and reinstalls when the binary's module version or the Go that built
  it differs from what's wanted (read with `go version -m`). It ignores whatever `<name>` is on `PATH`: a staticcheck
  built by an older Go can't read a newer stdlib's export data and fails with "export data version 5 is greater than
  maximum supported version 4", which is what the Go 1.27.2 bump did to a `~/go/bin` copy. Renovate bumps the literals
  through the custom manager in `renovate.json`.
- **staticcheck and Go move together.** Go 1.27.2 changed the export data format, and only staticcheck 2026.2.2
  (`v0.8.2`) reads it. So `.mise.toml` pins Go to an exact patch release and staticcheck to a build that reads it;
  bumping one without the other fails the `staticcheck` check. Renovate groups the two for that reason. Today's pin is a
  pseudo-version (a 2026-10-09 commit, the one Cmdr pins), because 2026.2.2 was inside the three-day window when Go
  1.27.2 had to land, and staying on 1.27.1 would have failed `govulncheck` on eight stdlib advisories.
- **`cargo-audit`** (nickname `cargo-audit`) checks `Cargo.lock` against the RustSec database: a vulnerability fails, an
  unmaintained or unsound warning warns. `cargoAuditIgnoredAdvisories` holds the accepted ones, each with why it doesn't
  reach the shipped app and when to drop it. `cargo-audit` itself installs at a pinned `--version`.
- **`govulncheck`** (nickname `govulncheck`) is the Go twin, and only flags advisories our code actually calls into. An
  advisory whose fix is under three days old can't be adopted yet, so it warns, and turns red three days after it
  becomes adoptable.
- Both run in every local `./scripts/check.sh` and daily in `.github/workflows/advisories.yml`, so a new advisory for a
  dependency nobody touched still shows up within a day. They're deliberately not in `ci.yml`: a fresh advisory would
  turn every unrelated push red.
- **`cargo-deny`** (nickname `cargo-deny`) is the opposite case: it checks licenses, sources, and wildcard requirements
  against `deny.toml`, all of which depend only on the commit, so it gates in `ci.yml`'s Linux Rust job. It runs
  `check bans licenses sources` and never `advisories`, which stay with cargo-audit (`deny.toml`'s header says why). It
  installs `cargoDenyVersion` into the repo's `target/check-cargo-tools` and reinstalls when `--version` disagrees,
  ignoring any `cargo-deny` on `PATH`, the way `EnsureGoTool` does. It parses the `--format json` stream to count, and
  on failure reruns the human report at `--log-level error`, which carries the inclusion graph that says what pulls a
  crate in. It passes `--deny unmatched-skip --deny unnecessary-skip` (`cargoDenyLintLevels`) to both runs, because
  `deny.toml` can't raise those lints itself: with them, a stale `[bans] skip` entry fails just like an unlisted
  duplicate does, so the skip list can't drift from the graph. Run `cargo deny check` by hand and they're only warnings.
- **`[[licenses.clarify]]` hashes are cargo-deny's own**: xxHash32 (seed 0) of the license file after every line ending
  is normalized to one `\n`, with a trailing `\n`. A raw-bytes hash of a CRLF file won't match, and a clarify that
  doesn't match is skipped silently, so the crate falls back to its manifest license with no hint why.

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

The runner captures nextest's output and prints it when the check finishes, so a run that never finishes prints nothing
at all. `.config/nextest.toml` therefore sets `slow-timeout` with `terminate-after`: a stuck test is killed and named
rather than taking the whole CI leg to its job timeout and leaving no evidence.

`PRVW_TEST_NO_FAIL_FAST=1` adds `--no-fail-fast` to the nextest run (`nextestArgs`). nextest cancels the whole run on
the first test failure, which is what you want day to day and the opposite of what you want on a platform with no track
record: one cycle reports one defect and leaves the other 900 unexecuted. CI's Windows job sets it and the other two
don't. The lever is read in the runner rather than in a shell, so it reaches nextest the same way through `check.sh`,
`check.ps1`, and the `check.exe` the Windows job runs directly.

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
