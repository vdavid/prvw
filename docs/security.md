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
  `@latest`; `cargo-audit` at a pinned `--version`; `cargo-deny` and `cargo-about` at the `cargoDenyVersion` and
  `cargoAboutVersion` constants; CI's `cargo-nextest` by URL version; NSIS by `choco --version`. Custom managers in
  `renovate.json` find all but NSIS. `scripts/check/CLAUDE.md` § Pinned tools has the mechanics.
- **Toolchains:** `rust-toolchain.toml` pins an exact Rust; `.mise.toml` pins Go, Node, and pnpm.

## Licenses, sources, and version requirements

`cargo-deny` checks the Rust workspace against `deny.toml` (nickname `cargo-deny`), in every local `./scripts/check.sh`
and in `ci.yml`'s Linux Rust job, where it gates. It can gate because its inputs are `Cargo.lock` and `deny.toml` alone,
so nothing changes under an untouched commit.

- **Licenses:** an explicit allowlist of permissive licenses, because Prvw is BSL-1.1 and statically links every crate
  into the binary it ships. Copyleft fails unless a per-crate `exceptions` entry says why it's fine. Today that's
  `rawler` (LGPL-2.1-only) and `lensfun` (LGPL-3.0-or-later): Prvw's public source is what lets a user relink against a
  modified copy, which is the obligation static linking brings. `[[licenses.clarify]]` pins a license where a crate's
  manifest is ambiguous, keyed to the license file's hash.
- **Every platform's graph:** `[graph] targets` lists the macOS, Windows, and Linux triples, so a Windows- or Linux-only
  crate gets checked from a Mac too.
- **What it can't see:** data under another license compiled into a crate. `lensfun` embeds the LensFun lens database,
  which is CC-BY-SA-3.0 and needs attribution, and its manifest only names the code's license. The notices below credit
  it by hand.
- **Sources:** crates.io only. A git dependency or another registry fails until `deny.toml` allows it.
- **Bans:** a `*` version requirement fails. So does a crate resolved in two versions, unless `[bans] skip` lists the
  old one with what pulls it in. The list is the exact set of known duplicates, all waiting on upstream: a new one fails
  until someone converges it (`cargo update -p <crate>@<old> --precise <ver>`) or lists it, and a listed one that
  upstream has since converged fails until its entry goes. Either way it's a decision, and the check stays silent in
  between. `deny.toml` § bans has the reasoning.
- npm has no equivalent check yet, so a new npm package still needs its license read by hand. The website is the only
  npm consumer, and none of it ships in the app.

## Third-party notices

Allowing a license isn't the whole obligation: MIT, Apache, BSD, the LGPL, and CC BY-SA all want their text and
copyright lines to travel with the binary. `THIRD-PARTY-NOTICES.md` at the repo root is that file, and it ships as
`THIRD-PARTY-NOTICES.txt` in the `.app`'s `Resources/` (`scripts/build-and-sign.sh`) and the Windows install folder
(`prvw.nsi`), beside `LICENSE.txt`. The About box links to the copy at the release's tag.

- **Generated, never hand-edited:** the `third-party-notices` check runs cargo-about over the app crate, for the same
  six targets and the same accepted licenses `deny.toml` lists, and merges in `third-party-vendored.json`: the material
  no crate's license field describes (the LensFun database, the RawTherapee DCP profiles). Locally it rewrites the file;
  in CI it fails when the committed copy is stale. `scripts/check/CLAUDE.md` § The third-party notices check has the
  mechanics.
- **Crates are listed without versions,** so a patch bump that adds no crate regenerates the same bytes and a Renovate
  PR can still pass CI. A bump that pulls in a new crate does change the file, and needs a local run.
- **The LGPL paragraph** at its top says what the two LGPL crates entitle a user to (rebuilding against a modified copy)
  and where Prvw's source is, which is the reason `deny.toml` can accept them.

## Advisory scanning

- **`cargo-audit`** checks `Cargo.lock` against RustSec. Accepted advisories live in `cargoAuditIgnoredAdvisories`
  (`scripts/check/checks/desktop-rust-cargo-audit.go`), each with why it doesn't reach the shipped app and when to drop
  it. ❌ Don't add one without that reasoning. cargo-deny could check advisories too, and deliberately doesn't: keeping
  them in a separate tool keeps the gating lane off the daily-changing database, and keeps cargo-audit's split between a
  failing vulnerability and a warning-only unmaintained crate, which cargo-deny would make errors.
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
