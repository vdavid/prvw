Prepare a release based on docs/guides/releasing.md.

1. Prerequisite: Run `gh secret list` and verify that these secrets exist: `APPLE_CERTIFICATE`,
   `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_API_KEY`, `APPLE_API_KEY_BASE64`, `APPLE_API_ISSUER`.
   If any are missing, warn the user and stop.
2. Update @CHANGELOG.md based on git commits since last release.
   - Read the file first to match its style.
   - Commits have title + body - read all!
   - You can link multiple commits for changelog items if needed.
   - **Get commit SHAs via `git log --format='%h' --abbrev=8`** — never extend a 7-char prefix from `git log
     --oneline` by guessing the next character. The committed changelog convention is 8 chars; let git produce them.
     The `changelog-links` check (run by the release script) will reject fabricated SHAs and abort the release.
   - **Add a `## [Unreleased]` heading** right after the format preamble (before the first versioned section), then put
     entries under it. The release script replaces this heading with the versioned one. The committed changelog has no
     `[Unreleased]` section between releases - you're creating it fresh each time.
3. Based on the changes, advise what the next version should be (patch: bug fixes, minor: new features, major: major
   launches), and give the user the `./scripts/release.sh x.x.x` command to run.
4. **Offer to run the release script** for the user. Wait for confirmation before running.
5. **Offer to push** with `git push origin main --tags`. Wait for confirmation before pushing.
6. **After pushing**, confirm the self-hosted runner picked up the build:
   - Wait ~30 seconds, then run `gh run view <release-run-id> --json jobs` and check the `Build (...)` jobs.
   - At least one `Build (...)` job should be `in_progress` (the self-hosted runner serializes the three matrix jobs,
     so the others stay `queued` — that's normal).
   - **If all three are still `queued` after ~30s, the self-hosted runner is down.** Follow the recovery procedure in
     [docs/guides/releasing.md § Runner-up sanity check](../../docs/guides/releasing.md#runner-up-sanity-check-during-a-release):
     `launchctl list | grep prvw` to confirm, then `cd ~/actions-runner-prvw && ./svc.sh start`, falling back to
     `launchctl bootout` + `bootstrap` if `svc.sh` errors with "Load failed: 5: Input/output error". Re-check after
     another 30s. The queued jobs pick up automatically once the runner reports in — no need to re-trigger or re-tag.
7. **Then start monitoring the CI build**:
   - Remind the user not to close their laptop for ~15-25 minutes while the self-hosted runner builds (three archs
     sequentially).
   - Poll `gh run view` every few minutes in the background and report progress (which jobs are done, which are still
     running).
   - Report when all jobs complete (success or failure). If a job fails, show the failure details, and advise how to
     fix.
   - Suggest the user to also track the build at https://github.com/vdavid/prvw/actions.
8. **In parallel, watch the standalone CI run** (the non-release `CI` workflow that fires on the same push):
   - It's not a blocker for the release. If it goes red, fix it in the background while the release builds — small
     things like lint regressions are common.
   - Surface the failure to the user when convenient; don't interrupt release-build progress reporting for it.
