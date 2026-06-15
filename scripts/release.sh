#!/bin/bash
set -euo pipefail

# Prepare a local release of Prvw: bumps version, finalises CHANGELOG, commits,
# tags. Pushing the tag triggers `.github/workflows/release.yml`.
#
# Operators (humans and AI agents): NEVER tail, head, or filter this script's
# output. It's already concise and the warnings carry weight — hiding them is
# how releases break silently. Run it in a terminal that shows everything.

VERSION="${1:-}"

if [[ -z "$VERSION" ]]; then
  echo "Usage: ./scripts/release.sh <version>"
  echo "Example: ./scripts/release.sh 0.1.0"
  exit 1
fi

# Validate version format
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Error: Version must be in format X.Y.Z (e.g., 0.1.0)"
  exit 1
fi

# Check for uncommitted changes (CHANGELOG.md is allowed — it gets included in the release commit)
EXCLUDE=(':!CHANGELOG.md')
if ! git diff --quiet -- "${EXCLUDE[@]}" || ! git diff --staged --quiet -- "${EXCLUDE[@]}"; then
  echo "Error: Working tree has uncommitted changes (other than CHANGELOG.md). Commit them first."
  exit 1
fi

# Stage CHANGELOG.md before rebase so it doesn't block it
git add CHANGELOG.md 2>/dev/null || true

# Pull latest main to avoid push rejection after tagging
# --autostash: temporarily stashes staged changelog changes so rebase can proceed
git pull --rebase --autostash origin main

# Check CHANGELOG.md has an [Unreleased] section with content
if ! grep -q '## \[Unreleased\]' CHANGELOG.md; then
  echo "Error: CHANGELOG.md has no [Unreleased] section."
  echo "Add a '## [Unreleased]' heading with release notes before the first versioned section."
  exit 1
fi
UNRELEASED_CONTENT=$(sed -n '/## \[Unreleased\]/,/## \[/p' CHANGELOG.md | sed '1d;$d' | grep -v '^$' || true)
if [[ -z "$UNRELEASED_CONTENT" ]]; then
  echo "Error: The [Unreleased] section in CHANGELOG.md is empty."
  echo "Add release notes under it before releasing!"
  exit 1
fi

# Pre-flight sanity (auto-fix where safe). All gates here have bitten a real
# release at some point — keep them.

# 1) Detach stale Prvw* DMG mounts. A leftover mount makes macOS auto-rename
#    the new volume to "Prvw 1", and TCC blocks the runner from writing inside
#    it ("Operation not permitted"). create-dmg.sh has the same guard, but
#    failing fast here means we don't tag a release that can't ship.
while IFS= read -r vol; do
  if [[ -n "$vol" ]]; then
    echo "Detaching stale mount: $vol"
    hdiutil detach "$vol" -force >/dev/null 2>&1 || true
  fi
done < <(mount | awk -F' on ' '/\/Volumes\/Prvw/ { sub(/ \(.*$/, "", $2); print $2 }')

# 2) Self-hosted GitHub Actions runner up. Without it, jobs sit in `queued`
#    forever after we push the tag. Auto-fix the LaunchAgent if down. See
#    docs/guides/releasing.md § Runner-up sanity check for the manual recovery.
RUNNER_LABEL="actions.runner.vdavid-prvw.Davids-M3-MBP-prvw"
RUNNER_LINE=$(launchctl list 2>/dev/null | awk -v label="$RUNNER_LABEL" '$3 == label { print; exit }')
if [[ -z "$RUNNER_LINE" ]]; then
  echo "Warning: $RUNNER_LABEL LaunchAgent not registered. Skipping runner check."
elif [[ "$(echo "$RUNNER_LINE" | awk '{ print $1 }')" == "-" ]]; then
  echo "Runner is down (PID = -). Starting via svc.sh..."
  if [[ -d "$HOME/actions-runner-prvw" ]]; then
    (cd "$HOME/actions-runner-prvw" && ./svc.sh start)
    sleep 2
    RUNNER_LINE=$(launchctl list 2>/dev/null | awk -v label="$RUNNER_LABEL" '$3 == label { print; exit }')
    if [[ "$(echo "$RUNNER_LINE" | awk '{ print $1 }')" == "-" ]]; then
      echo "Error: runner is still down after svc.sh start. See docs/guides/releasing.md § Runner-up sanity check."
      exit 1
    fi
    echo "Runner started: PID $(echo "$RUNNER_LINE" | awk '{ print $1 }')"
  else
    echo "Error: $HOME/actions-runner-prvw not found. Cannot auto-fix runner."
    exit 1
  fi
else
  echo "Runner is up (PID $(echo "$RUNNER_LINE" | awk '{ print $1 }'))."
fi

# 3) Clean node_modules so the check suite installs from scratch. A warm
#    node_modules hides install-config breakage: `./scripts/check.sh` skips the
#    pnpm install when the lockfile is unchanged, so a fresh-install failure
#    (e.g. pnpm's ERR_PNPM_IGNORED_BUILDS, which only fires on a clean install)
#    would otherwise sail through local checks and only surface in CI or the
#    website deploy. The full suite below repopulates node_modules.
echo "Removing node_modules to force a clean install in the checks..."
rm -rf node_modules apps/*/node_modules

# 4) Full check suite. Catches lint, format, and test regressions, plus the
#    `changelog-links` validator that flags fabricated commit SHAs in the
#    [Unreleased] section before we tag.
echo "Running ./scripts/check.sh (full suite)..."
./scripts/check.sh

echo "Releasing version $VERSION..."

# Update version in Cargo.toml and sync Cargo.lock
sed -i '' "s/^version = \".*\"/version = \"$VERSION\"/" apps/desktop/Cargo.toml
cargo update --workspace --quiet

# Update CHANGELOG.md: replace [Unreleased] with the versioned heading
TODAY=$(date +%Y-%m-%d)
sed -i '' "s/## \[Unreleased\]/## [$VERSION] - $TODAY/" CHANGELOG.md

# Commit and tag (only files touched by this script)
git add \
  CHANGELOG.md \
  apps/desktop/Cargo.toml \
  Cargo.lock
git commit -m "chore(release): v$VERSION"
git tag "v$VERSION"

echo ""
echo "Release v$VERSION prepared locally."
echo "To publish, run: git push origin main --tags"
