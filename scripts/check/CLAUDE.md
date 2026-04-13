# Check runner

Go CLI that runs all code quality checks for the Prvw monorepo in parallel with dependency ordering. Ported from
[Cmdr's check runner](https://github.com/vdavid/cmdr/tree/main/scripts/check). Invoked via `./scripts/check.sh`.

## Quick start

```bash
./scripts/check.sh                    # All checks
./scripts/check.sh --app desktop      # Desktop (Rust) only
./scripts/check.sh --check clippy     # Specific check
./scripts/check.sh --rust             # All Rust checks
./scripts/check.sh --svelte           # All Svelte checks
./scripts/check.sh --go               # All Go checks
./scripts/check.sh --ci --fail-fast   # CI mode
```

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

| File                        | Purpose                                                                 |
| --------------------------- | ----------------------------------------------------------------------- |
| `main.go`                   | Entry point: flags, root dir, check selection, runner delegation        |
| `runner.go`                 | Parallel executor: goroutine pool, dependency graph, TTY status line    |
| `checks/common.go`          | Core types, shared utils (`RunCommand`, `EnsureGoTool`, `runESLintCheck`) |
| `checks/registry.go`        | `AllChecks`: canonical ordered list, lookup and validation functions    |
| `checks/desktop-rust-*.go`  | Rust checks (rustfmt, clippy, cargo-test)                              |
| `checks/desktop-svelte-*.go` | Svelte checks (oxfmt, prettier, eslint, stylelint, svelte-check, build) |
| `checks/desktop-oxfmt.go`  | oxfmt formatting for the desktop frontend                              |
| `checks/website-*.go`       | Website checks (prettier, eslint, typecheck, build)                    |
| `checks/scripts-go-*.go`    | Go checks (gofmt, go-vet, staticcheck, misspell, gocyclo, deadcode, tests) |
| `stats.go`                  | CSV stats logging (`~/prvw-check-log.csv`)                             |
| `colors.go`                 | ANSI color constants                                                   |
| `utils.go`                  | `findRootDir()` (walks up until `AGENTS.md` is found)                  |

## Adding a new check

1. Create `checks/{app}-{name}.go` with `func RunSomething(ctx *CheckContext) (CheckResult, error)`.
2. Register in `AllChecks` in `registry.go`.
3. Return `Success("message")` on pass, `fmt.Errorf(...)` on fail, `Skipped("reason")` to skip.
4. Run `./scripts/check.sh --go` to verify.

## Key patterns

- **Graceful skipping**: Rust and website checks skip if their directory/`Cargo.toml` doesn't exist yet.
- **Auto-fix vs CI**: `--ci` disables auto-fixing. Formatters fix locally, report-only in CI.
- **IDs vs nicknames**: `--check` accepts either. `CLIName()` returns nickname if set, else ID.
- **CSV stats**: Each run appends to `~/prvw-check-log.csv`. Disabled by `--no-log` or `--ci`.

## Apps and checks

| App     | Tech   | Checks                                                    |
| ------- | ------ | --------------------------------------------------------- |
| Desktop | Rust   | rustfmt, clippy, cargo-test                               |
| Desktop | Svelte | oxfmt, prettier, eslint, stylelint, svelte-check, build   |
| Website | Astro  | prettier, eslint, typecheck, build                        |
| Scripts | Go     | gofmt, go-vet, staticcheck, misspell, gocyclo, deadcode, tests |
