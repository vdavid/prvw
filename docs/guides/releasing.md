# Releasing

How to release a new version of Prvw. Use the `/release` command to start.

## Prerequisites

- GitHub secrets configured (same Apple Developer account as Cmdr):
  - `APPLE_CERTIFICATE` and `APPLE_CERTIFICATE_PASSWORD` (code signing)
  - `APPLE_SIGNING_IDENTITY` (`Developer ID Application: Rymdskottkarra AB (83H6YAQMNP)`)
  - `APPLE_API_KEY`, `APPLE_API_KEY_BASE64`, `APPLE_API_ISSUER` (notarization)
- Self-hosted runner tagged `[self-hosted, macOS, ARM64]`

## What the release does

1. `scripts/release.sh` bumps version in `Cargo.toml`, updates `CHANGELOG.md`, commits, and tags
2. Pushing the `v*` tag triggers `.github/workflows/release.yml`
3. The workflow builds aarch64, x86_64, and universal binaries
4. Each binary is signed with hardened runtime, packaged into a DMG, notarized, and stapled
5. A GitHub Release is created with all three DMGs attached

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
