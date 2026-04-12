#!/bin/bash
set -euo pipefail

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
