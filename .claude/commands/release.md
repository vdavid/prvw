Prepare a release based on docs/guides/releasing.md.

1. Prerequisite: Run `gh secret list` and verify that these secrets exist: `APPLE_CERTIFICATE`,
   `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_API_KEY`, `APPLE_API_KEY_BASE64`,
   `APPLE_API_ISSUER`. If any are missing, warn the user and stop.
2. Update @CHANGELOG.md based on git commits since last release.
   - Read the file first to match its style.
   - Commits have title + body - read all!
   - You can link multiple commits for changelog items if needed.
   - **Add a `## [Unreleased]` heading** right after the format preamble (before the first versioned section), then put
     entries under it. The release script replaces this heading with the versioned one. The committed changelog has no
     `[Unreleased]` section between releases - you're creating it fresh each time.
3. Based on the changes, advise what the next version should be (patch: bug fixes, minor: new features, major: major
   launches), and give the user the `./scripts/release.sh x.x.x` command to run.
4. **Offer to run the release script** for the user. Wait for confirmation before running.
5. **Offer to push** with `git push origin main --tags`. Wait for confirmation before pushing.
6. **After pushing**, start monitoring the CI build:
   - Remind the user not to close their laptop for ~15 minutes while the self-hosted runner builds.
   - Poll `gh run view` every few minutes in the background and report progress (which jobs are done, which are still
     running).
   - Report when all jobs complete (success or failure). If a job fails, show the failure details, and advise how to
     fix.
   - Suggest the user to also track the build at https://github.com/vdavid/prvw/actions.
