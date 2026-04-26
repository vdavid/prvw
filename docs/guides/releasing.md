# Releasing

How to release a new version of Prvw. Use the `/release` command to start.

## Prerequisites

- GitHub secrets configured (same Apple Developer account as Cmdr):
  - `APPLE_CERTIFICATE` and `APPLE_CERTIFICATE_PASSWORD` (code signing)
  - `APPLE_SIGNING_IDENTITY` (`Developer ID Application: Rymdskottkarra AB (83H6YAQMNP)`)
  - `APPLE_API_KEY`, `APPLE_API_KEY_BASE64`, `APPLE_API_ISSUER` (notarization)
- Self-hosted runner tagged `[self-hosted, macOS, ARM64]` running on David's M3 MacBook Pro (see
  [Self-hosted runner](#self-hosted-runner) below)
- `CHANGELOG.md` `[Unreleased]` section populated per [docs/guides/changelog.md](changelog.md) (entries concise +
  commit-linked, validated by the `changelog-commit-links` check)

## What the release does

1. `scripts/release.sh` bumps version in `Cargo.toml`, updates `CHANGELOG.md`, commits, and tags
2. Pushing the `v*` tag triggers `.github/workflows/release.yml`
3. The workflow builds aarch64, x86_64, and universal binaries
4. Each binary is signed with hardened runtime, packaged into a DMG, notarized, and stapled
5. A GitHub Release is created with all three DMGs attached
6. `apps/website/public/latest.json` is regenerated (with `version`, `pub_date`, per-arch DMG URLs, and `dmgSizes` in
   bytes) and committed to `main`, then the website redeploy webhook is fired

## Expected timing

The single self-hosted runner builds the three architectures sequentially. As of v0.11.0, each `Build (...)` job
takes ~7 minutes 30 seconds (compile + sign + notarise + staple), so the three together come in around **22 - 23
minutes** before the final `Release` job creates the GitHub Release. The app keeps growing — RAW pipeline,
LensFun database, bundled DCPs — so this number trends up over time. Re-measure when it feels off, don't trust
older estimates here.

## Self-hosted runner

The release workflow targets a self-hosted ARM64 macOS runner installed on David's M3 MacBook Pro at
`~/actions-runner-prvw/` (registered as
`actions.runner.vdavid-prvw.Davids-M3-MBP-prvw`). It runs as a per-user `launchd` LaunchAgent so it survives reboots
and login.

### Runner-up sanity check during a release

After pushing the tag, the three `Build (...)` jobs should leave `queued` within ~10 seconds. **If they're still
`queued` after 10-15 seconds, the runner is not up on the Mac.** Don't wait it out — the agent should fix it
immediately:

```bash
# 1) Confirm it's down: PID column will be `-` and the last exit code is non-zero (often -9).
launchctl list | grep prvw
# Expected when up:    65420   0   actions.runner.vdavid-prvw.Davids-M3-MBP-prvw
# When down:           -      -9   actions.runner.vdavid-prvw.Davids-M3-MBP-prvw

# 2) Try the bundled service script first.
cd ~/actions-runner-prvw && ./svc.sh start

# 3) If that fails with "Load failed: 5: Input/output error", the LaunchAgent is in a stuck state.
#    Bootout + bootstrap to clear it:
PLIST=~/Library/LaunchAgents/actions.runner.vdavid-prvw.Davids-M3-MBP-prvw.plist
UID_=$(id -u)
launchctl bootout gui/$UID_/actions.runner.vdavid-prvw.Davids-M3-MBP-prvw 2>/dev/null
launchctl bootstrap gui/$UID_ "$PLIST"

# 4) Verify it's listening.
launchctl list | grep prvw          # PID > 0, last exit 0
tail ~/actions-runner-prvw/_diag/Runner_*.log | tail -5
# Last line should read: "Listening for Jobs"
```

The queued release jobs will pick up automatically once the runner reports in — no need to re-trigger or re-tag.

### Why it sometimes goes down

The MacBook sleeping or restarting can leave the LaunchAgent loaded but its worker process exited. macOS doesn't
auto-restart from a stuck `Input/output error` state — the bootout + bootstrap pair clears it.

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

The release job downloads DMGs from artifacts and creates a GitHub Release. If it fails, re-run it. The build
artifacts are retained by GitHub Actions and will be re-downloaded.
