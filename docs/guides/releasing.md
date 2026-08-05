# Releasing

How to release a new version of Prvw. Use the `/release` command to start.

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
5. A GitHub Release is created with all three DMGs attached
6. `apps/website/public/latest.json` is regenerated (with `version`, `pub_date`, per-arch DMG URLs, and `dmgSizes` in
   bytes) and committed to `main`, then the website redeploy webhook is fired

## Expected timing

The three `Build (...)` jobs run in parallel on separate hosted runners, so wall-clock is roughly one build, not three.
Each is ephemeral and pays a cold compile, plus sign, notarise, and staple. Apple's notarisation dominates the tail and
varies from minutes to hours, which is why `timeout-minutes` is 150.

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

The Release job downloads the three DMGs from artifacts, creates the GitHub Release, commits the regenerated
`latest.json` to `main` (via `git pull --rebase origin main`), and fires the website-deploy webhook. The build artifacts
are retained by GitHub Actions, so re-running the job is safe:

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
`osascript` is a TCC client macOS may not have authorized; the first "control Finder" prompt blocks (and times out if
no one clicks Allow), after which create-dmg degrades to the unstyled fallback. If releases start shipping unstyled
DMGs while running self-hosted, trigger the prompt once while you're at the keyboard and click Allow:

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
