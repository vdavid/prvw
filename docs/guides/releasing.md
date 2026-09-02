# Releasing

How to release a new version of Prvw. Use the `/release` command to start.

**A tag ships macOS and Windows.** Everything from "Prerequisites" to the end of the troubleshooting section is the
macOS story: three signed, notarised DMGs. Windows gets one leg of its own that builds `PrvwSetup-<version>-x64.exe` and
attaches it to the same GitHub Release, unsigned until a certificate exists. [Windows](#windows) has that story,
including what's still open.

## Prerequisites

- GitHub secrets configured (same Apple Developer account as Cmdr):
  - `APPLE_CERTIFICATE` and `APPLE_CERTIFICATE_PASSWORD` (code signing)
  - `APPLE_SIGNING_IDENTITY` (`Developer ID Application: Rymdskottkarra AB (83H6YAQMNP)`)
  - `APPLE_API_KEY`, `APPLE_API_KEY_BASE64`, `APPLE_API_ISSUER` (notarization)
- Nothing machine-specific: the build runs on GitHub-hosted `macos-latest` (see
  [Which runner builds the release](#which-runner-builds-the-release) below)
- `CHANGELOG.md` `[Unreleased]` section populated per [docs/guides/changelog.md](changelog.md) (entries concise +
  commit-linked, validated by the `changelog-links` check)

## What the release does

1. `scripts/release.sh` bumps version in `Cargo.toml`, updates `CHANGELOG.md`, commits, and tags
2. Pushing the `v*` tag triggers `.github/workflows/release.yml`
3. The workflow builds aarch64, x86_64, and universal binaries
4. Each binary is signed with hardened runtime, packaged into a DMG, notarized, and stapled
5. In parallel, `windows-latest` builds `prvw.exe` and packages `PrvwSetup-<version>-x64.exe` with NSIS
6. A GitHub Release is created with all three DMGs and the Windows installer attached
7. `apps/website/public/latest.json` is regenerated (with `version`, `pub_date`, per-arch DMG URLs, `dmgSizes` in bytes,
   and a `windows-x86_64` platform entry carrying its own `size`) and committed to `main`, then the website redeploy
   webhook is fired

The Windows leg gates the release the same way the macOS ones do: if it fails, no GitHub Release appears.

## Expected timing

The three macOS `Build (...)` jobs and the Windows one run in parallel on separate hosted runners, so wall-clock is
roughly one build, not four. Each is ephemeral and pays a cold compile. The macOS legs then sign, notarise, and staple,
and Apple's notarisation dominates the tail, varying from minutes to hours, which is why their `timeout-minutes` is 150.
Windows packages instead of notarising, so its 90 is all compile and compression.

The numbers previously recorded here (~7m30s per job, ~22 minutes total) were measured on the self-hosted runner with a
warm cargo cache and don't transfer. Re-measure on the next release rather than trusting an estimate here.

## Which runner builds the release

Two runners can build Prvw, and the choice is one line in `release.yml` (`build.runs-on`).

**GitHub-hosted (`macos-latest`) is what runs today.** The self-hosted Mac at `~/actions-runner-prvw/` (registered as
`actions.runner.vdavid-prvw.Davids-M3-MBP-prvw`, a per-user `launchd` LaunchAgent) is registered but not the target.

- **Why hosted won**: releases stop depending on one laptop being awake, online, and un-slept for the whole build. The
  self-hosted runner dropped its connection whenever the Mac idle-slept, failing every in-flight job with
  `The self-hosted runner lost communication with the server`, so every release needed a babysat `caffeinate`. Hosted
  runners also sidestep the Finder/TCC Automation gate that unstyles DMGs (see the DMG troubleshooting section below).
  The repo is public, so hosted macOS minutes are free.
- **What hosted costs**: every job is ephemeral, so each pays a cold cargo compile instead of reusing a warm cache.
  Partly offset by the three arch jobs running in parallel rather than queueing on one machine.

**To switch back**: set `runs-on: [self-hosted, macOS, ARM64]` and restart the service with
`cd ~/actions-runner-prvw && ./svc.sh start`. If that fails with `Load failed: 5: Input/output error` the LaunchAgent is
stuck; clear it with a bootout + bootstrap pair:

```bash
PLIST=~/Library/LaunchAgents/actions.runner.vdavid-prvw.Davids-M3-MBP-prvw.plist
UID_=$(id -u)
launchctl bootout gui/$UID_/actions.runner.vdavid-prvw.Davids-M3-MBP-prvw 2>/dev/null
launchctl bootstrap gui/$UID_ "$PLIST"
launchctl list | grep prvw   # PID > 0, last exit 0
```

The workflow still carries its self-hosted-specific guards (the stale-`/Volumes/Prvw` detach, the keychain search-list
restore), so nothing else has to change. Restore the `caffeinate` discipline in `/release` too, or sleep will keep
killing builds.

## Windows

The Windows installer is `PrvwSetup-<version>-x64.exe`, built by NSIS. Releases build it on `windows-latest`, and
`scripts/build-windows-installer.sh` builds the same thing from a Mac on demand: `makensis` runs natively on macOS, so a
local artifact comes off the same Mac that cross-compiles the exe, with no Windows machine involved.

### In the release workflow

The `Build (Windows installer)` job in `.github/workflows/release.yml` runs on `windows-latest`, and it's the same
script the Mac runs, handed a natively built exe:

1. `cargo build --release --target x86_64-pc-windows-msvc`, so no `cargo-xwin` and no `llvm-lib` shim are involved.
2. `choco install nsis --version 3.12.0`, then `C:\Program Files (x86)\NSIS` onto `GITHUB_PATH`. The chocolatey package
   runs the real NSIS setup, which installs into Program Files and leaves PATH alone, so `makensis` needs that line to
   be findable. Windows Server 2022 had NSIS preinstalled; the 2025 image behind `windows-latest` doesn't.
3. `./scripts/build-windows-installer.sh --exe target/x86_64-pc-windows-msvc/release/prvw.exe`, under `shell: bash`. The
   runner's bash is Git Bash, which speaks `/d/a/...` paths that `makensis` can't read, so the script translates them
   with `cygpath` before the `makensis` call.
4. A check that `PrvwSetup-<tag version>-x64.exe` exists, which catches a tag that disagrees with
   `apps/desktop/Cargo.toml`.

The installer then rides to the release job as the `windows-installer` artifact, gets attached to the GitHub Release
alongside the DMGs, and lands in `latest.json` as `platforms["windows-x86_64"]` with a `url` and a `size` in bytes. The
macOS updater ignores that key: it deserializes `platforms` into a map and asks for `darwin-<arch>`.

### Proving it before you tag

`.github/workflows/windows-installer.yml` runs the same two steps on `windows-latest` outside a release: a release build
of `prvw.exe`, then `scripts/build-windows-installer.sh` over it, with the installer uploaded as the `windows-installer`
artifact. It fires on any push that touches `apps/desktop/installer/**`, `scripts/build-windows-installer.sh`, or
`xtask/**`, and on `gh workflow run windows-installer.yml --ref <branch>`.

It exists because CI's `installer` check needs no makensis (it compares generated text and reads two facts out of
`prvw.nsi`), so nothing used to compile the script on Windows until a tag did. Run it on a branch before tagging if
you've touched anything the installer reads.

### Build one

```bash
brew install makensis                     # once; apt install nsis on Linux
./scripts/check.sh --check windows-cross  # once, to create target/cross-check-bin/llvm-lib
./scripts/build-windows-installer.sh
```

That cross-builds `prvw.exe` with `cargo-xwin`, regenerates the file-type registration, and writes
`target/windows-installer/PrvwSetup-<version>-x64.exe`. About 17 MB from a 36 MB executable, and a couple of minutes
cold. Pass `--exe <path>` to package a binary someone else built, which is what the release workflow's Windows leg does.

The version comes from `apps/desktop/Cargo.toml` and nowhere else: the script reads it, the exe's own version info comes
from the same field through `build.rs`, and `./scripts/check.sh --check installer` fails if `prvw.nsi` ever spells a
version out.

### What the installer does

- Installs per user into `$LOCALAPPDATA\Programs\Prvw`, so there's no UAC prompt at any point.
- Adds a `Prvw` shortcut to the Start menu and an entry to Apps & features, both under `HKCU`.
- Registers Prvw's file types: a ProgID, `OpenWithProgids` on every extension the decoder handles, and a `Capabilities`
  block that gives Prvw its own page under Settings → Apps → Default apps. It can't _set_ the default, and nothing can:
  Windows 10 20H2 took that away from apps. The user picks.
- Refuses to install over a running Prvw, with a retry.
- Uninstalls cleanly, including taking the file types back out.

`apps/desktop/installer/windows/CLAUDE.md` has the decisions and the gotchas.

### Signing, which releases don't do yet

Releases ship an unsigned installer. Azure Trusted Signing is the plan (M1 step 16 in
`docs/specs/cross-platform-plan.md`), and the account is David's to set up.

The seam is one variable. `scripts/build-windows-installer.sh` runs `$PRVW_WINDOWS_SIGN_CMD <file>` when that variable
is set, once for `prvw.exe` before packaging and once for the finished installer, and does nothing when it's empty. The
release job passes it through from the `PRVW_WINDOWS_SIGN_CMD` **repository variable**, which doesn't exist today, so
the env var arrives empty and the build stays unsigned:

```bash
gh variable set PRVW_WINDOWS_SIGN_CMD --body 'D:\a\sign-one-file.cmd'
```

Locally it's the same variable:

```bash
PRVW_WINDOWS_SIGN_CMD=/path/to/sign-one-file.sh ./scripts/build-windows-installer.sh
```

The command has to sign in place and exit non-zero on failure. Trusted Signing also needs a step ahead of the build that
installs its dispatcher and hands it credentials from repository secrets, the way the Apple certificate import already
works. Two more things to know when the account exists:

- **Signing needs a Windows host**, which the release leg already is. `signtool` and the Trusted Signing dispatcher are
  Windows binaries, so a signing command can't come from the Mac build path, only from `windows-latest`.
- **The uninstaller stays unsigned** with a single pass. NSIS generates `Uninstall Prvw.exe` by running the installer at
  build time, so signing it means building twice: build once, run the installer with `/S` into a scratch directory to
  extract the uninstaller, sign that, then build again with the signed copy included as a plain `File`. Worth doing
  before the first paid release; not worth doing before there's a certificate.

Expect SmartScreen warnings for the first few hundred downloads whatever the certificate says. EV certificates lost
their reputation bypass in 2024.

### What's left before Windows can ship

1. Trusted Signing in the build leg, through the `PRVW_WINDOWS_SIGN_CMD` variable above. Until then every download meets
   SmartScreen with no publisher behind it.
2. The Windows updater that reads `latest.json` (M7 step 4). The release workflow writes a `windows-x86_64` entry into
   the manifest, and the committed copy won't carry one until the first Windows release generates it.
3. One pass through the installer on a real Windows box. Nothing in it has ever run; the list of what to watch is at the
   bottom of `apps/desktop/installer/windows/CLAUDE.md`.
4. A Windows download on getprvw.com. `src/lib/download.ts` names the three `darwin-*` keys and stops there.
5. A tagged release that carries the installer all the way to the GitHub Release. The Windows leg has run and now
   packages successfully, but no release has published its output yet.

## Troubleshooting

### Release build failed, need to retry same version

Delete tag, fix the issue, commit, recreate tag, push:

```bash
git tag -d v0.x.x                      # delete local tag
git push origin :refs/tags/v0.x.x      # delete remote tag
# ... fix and commit ...
git tag v0.x.x                         # recreate tag
git push origin main --tags            # push again
```

### Apple notarization is slow (builds time out at 30 min)

Apple's notarization can take anywhere from minutes to 20+ hours. If the build job times out, the release job won't
run - no broken state.

The submission ID is logged in the build output. Once the status shows `Accepted`, re-run the failed job(s) in GitHub
Actions. Apple will return `Accepted` immediately (same binary hash), and the build will complete in minutes.

Use "Re-run failed jobs" (not "Re-run all jobs") to avoid rebuilding architectures that already succeeded.

### Release job failed but builds succeeded

The Release job downloads the three DMGs and the Windows installer from artifacts, creates the GitHub Release, commits
the regenerated `latest.json` to `main` (via `git pull --rebase origin main`), and fires the website-deploy webhook. The
build artifacts are retained by GitHub Actions, so re-running the job is safe:

- **Push race:** another commit landed on `main` between the job's checkout and its push. Re-running handles this — it
  rebases first. If the rebase itself conflicts (someone else edited `latest.json`), resolve it manually.
- **Webhook failed but `latest.json` is already on `main`:** the GitHub Release is live and users can download, but
  `getprvw.com/latest.json` is stale, so the in-app updater won't see the new version. Re-trigger the website-deploy
  workflow via `workflow_dispatch` from the Actions tab, or push any commit to `main`. This doesn't block release
  success — the GitHub Release is what users actually download.

### Website shows the old version even though the deploy step "succeeded"

The deploy is a server-side build: the `Trigger website deploy` step POSTs a signed payload to
`https://getprvw.com/hooks/deploy-website`, an `adnanh/webhook` systemd service on Hetzner
(`deploy-prvw-webhook.service`, port 9001) that **returns 2xx on receipt and then runs `docker build` asynchronously**.
So a green GitHub step only means the webhook was accepted — the actual site build can still fail silently afterward.

Confirm by checking the served file against the origin: a stale `last-modified` header
(`curl -sI https://getprvw.com/latest.json`) pointing at the previous release's date, with `cf-cache-status: DYNAMIC`,
means it's the origin serving an old file, not a CDN cache. Then read the build log on the server:

```bash
ssh hetzner "journalctl -u deploy-prvw-webhook.service --no-pager -n 60"
```

A failed `docker build` here is what keeps the site on the old version. One cause already hit and fixed: pnpm 11 turns
"ignored build scripts" into a hard `pnpm install` error, so `apps/website/Dockerfile` installs with `--ignore-scripts`
(see `apps/website/CLAUDE.md`). Fix the build, push to `main`, and the next webhook (or
`gh workflow run ci.yml --ref main`) redeploys.

### Release ships an unstyled DMG (or stalls in the DMG step)

`scripts/create-dmg.sh` styles the DMG with `create-dmg`, which drives Finder through `osascript`, and falls back to a
plain `hdiutil` DMG when Finder isn't reachable.

**This is a self-hosted-only failure — hosted images have no TCC gate.** On the self-hosted runner the bundled `node` /
`osascript` is a TCC client macOS may not have authorized; the first "control Finder" prompt blocks (and times out if no
one clicks Allow), after which create-dmg degrades to the unstyled fallback. If releases start shipping unstyled DMGs
while running self-hosted, trigger the prompt once while you're at the keyboard and click Allow:

```bash
NODE=~/actions-runner-prvw/externals/node20/bin/node
"$NODE" -e "require('child_process').execFileSync('/usr/bin/osascript', ['-e', 'tell application \"Finder\" to return name of startup disk'], { stdio: 'inherit' })"
```

From then on, every `osascript` call from that runner-node path is authorized until the runner auto-updates and changes
the node path.

### `create-dmg` fails fast on a leftover `/Volumes/Prvw` mount

Both `scripts/release.sh` and `scripts/create-dmg.sh` detach stale `/Volumes/Prvw*` mounts before building, so this is
normally self-healing. If you mount a release DMG between the detach and the build, the DMG step can fail fast on the
name clash. Detach manually and re-run the failed jobs:

```bash
hdiutil detach /Volumes/Prvw -force      # or "Prvw 1", etc.
gh run rerun <release-run-id> --failed
```
