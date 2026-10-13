# Security: supply chain

How Prvw keeps third-party code from changing under it. The common thread is that nothing gets in by name alone: every
dependency, tool, action, and image is pinned to an exact version or digest, waits three days after publication before
we adopt it, and gets scanned for advisories after it lands. Modeled on Cmdr's setup.

## The three-day window

A release that turns out to be malicious is usually yanked within a day or two, so we never adopt anything younger than
three days. It applies everywhere:

- **Rust crates, Go modules, the toolchains, Actions, and images:** Renovate's `minimumReleaseAge: "3 days"`
  (`renovate.json`).
- **npm packages:** pnpm's `minimumReleaseAge: 4320` (`pnpm-workspace.yaml`, which also carries `trustPolicy` and
  `blockExoticSubdeps`, each with its why).
- **By hand:** a manual upgrade checks publish dates first (crates.io `created_at`,
  `proxy.golang.org/<module>/@v/<v>.info`, GitHub release dates) and pins anything younger back.
- **The exception:** security fixes. Renovate's `vulnerabilityAlerts` and the `matchCategories: ["security"]` rule skip
  the weekly schedule and automerge on green CI. They still wait three days, because a "fix" can be the attack too.

## What's pinned, and where

- **GitHub Actions:** every `uses:` is a full commit SHA with the tag in a trailing comment (`# v7.0.1`), resolved from
  the real tag with `gh api repos/<owner>/<repo>/commits/<tag>`. A tag can be moved; a SHA can't. Renovate's
  `helpers:pinGitHubActionDigests` keeps new ones pinned and bumps SHA and comment together.
- **Docker images:** `FROM image:tag@sha256:...` in `apps/website/Dockerfile`; Renovate's `pinDigests` refreshes them.
- **The check runner's tools:** Go analyzers through `EnsureGoTool(..., "module/cmd/x@vX.Y.Z")`, which refuses
  `@latest`; `cargo-audit` at a pinned `--version`; CI's `cargo-nextest` by URL version; NSIS by `choco --version`.
  Custom managers in `renovate.json` find the first three. `scripts/check/CLAUDE.md` § Pinned tools has the mechanics.
- **Toolchains:** `rust-toolchain.toml` pins an exact Rust; `.mise.toml` pins Go, Node, and pnpm.

## Advisory scanning

- **`cargo-audit`** checks `Cargo.lock` against RustSec. Accepted advisories live in `cargoAuditIgnoredAdvisories`
  (`scripts/check/checks/desktop-rust-cargo-audit.go`), each with why it doesn't reach the shipped app and when to drop
  it. ❌ Don't add one without that reasoning.
- **`govulncheck`** checks the Go check runner, and only flags vulnerable code we actually call.
- **`pnpm audit`** covers the website's npm graph, run by hand (no check wraps it yet); `overrides` in
  `pnpm-workspace.yaml` hold the forced fixes.
- The first two run in every local `./scripts/check.sh` and daily in `.github/workflows/advisories.yml`, deliberately
  not in `ci.yml`, where a fresh advisory would turn every unrelated push red.

## Workflow hardening

- **Least privilege:** every workflow declares a read-only default at the top (`contents: read`, or `read-all` in
  Scorecard's, which its publish step expects). A job that needs more asks for it itself, with a comment saying why:
  only the `release` job writes (`contents: write`, for the GitHub release and the `latest.json` push), and only the
  CodeQL and Scorecard jobs upload to code scanning.
- **`persist-credentials: false`** on every checkout except the `release` job's, which pushes. Otherwise the token sits
  in `.git/config` for every later step to read.
- **Secrets enter a script through `env`,** never as `${{ secrets.X }}` inside `run:`, because an expression is pasted
  into the script text before the shell parses it.
- **CodeQL** (`codeql.yml`) analyzes the workflows and the Rust app weekly and on pushes that touch them. Findings go to
  the code-scanning tab; nothing gates on them yet.
- **OpenSSF Scorecard** (`scorecard.yml`) grades all of the above weekly and publishes the score. Its workflow has to
  stay minimal for the publish step to accept it; the comment at its top says what that means.
