#!/bin/bash
set -euo pipefail

# Creates a macOS .app bundle and packages it into a DMG.
# Usage: ./scripts/create-dmg.sh <binary_path> <output_dmg_path> <version> [info_plist_path] [icon_path]

BINARY_PATH="${1:-}"
OUTPUT_DMG="${2:-}"
VERSION="${3:-}"
INFO_PLIST="${4:-}"
ICON_PATH="${5:-}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DESKTOP_DIR="$PROJECT_ROOT/apps/desktop"

# Defaults for optional args
: "${INFO_PLIST:=$DESKTOP_DIR/Info.plist}"
: "${ICON_PATH:=$DESKTOP_DIR/resources/AppIcon.icns}"

APP_NAME="Prvw"

if [[ -z "$BINARY_PATH" || -z "$OUTPUT_DMG" || -z "$VERSION" ]]; then
  echo "Usage: ./scripts/create-dmg.sh <binary_path> <output_dmg_path> <version> [info_plist_path] [icon_path]"
  echo "Example: ./scripts/create-dmg.sh target/release/prvw Prvw_0.1.0_aarch64.dmg 0.1.0"
  exit 1
fi

if [[ ! -f "$BINARY_PATH" ]]; then
  echo "Error: Binary not found at $BINARY_PATH"
  exit 1
fi

if [[ ! -f "$INFO_PLIST" ]]; then
  echo "Error: Info.plist not found at $INFO_PLIST"
  exit 1
fi

if [[ ! -f "$ICON_PATH" ]]; then
  echo "Error: Icon not found at $ICON_PATH"
  exit 1
fi

TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

APP_BUNDLE="$TEMP_DIR/$APP_NAME.app"

echo "Creating .app bundle at $APP_BUNDLE..."

# Create bundle structure
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

# Copy binary
cp "$BINARY_PATH" "$APP_BUNDLE/Contents/MacOS/prvw"
chmod +x "$APP_BUNDLE/Contents/MacOS/prvw"

# Copy Info.plist with version replacement
sed "s/__VERSION__/$VERSION/g" "$INFO_PLIST" > "$APP_BUNDLE/Contents/Info.plist"

# Copy icon
cp "$ICON_PATH" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"

echo "Bundle structure:"
find "$APP_BUNDLE" -type f | sort

# Create Applications symlink for drag-to-install
ln -s /Applications "$TEMP_DIR/Applications"

# Remove existing DMG if present (hdiutil won't overwrite)
rm -f "$OUTPUT_DMG"

echo "Creating DMG: $OUTPUT_DMG..."
hdiutil create \
  -volname "$APP_NAME" \
  -srcfolder "$TEMP_DIR" \
  -ov \
  -format UDZO \
  "$OUTPUT_DMG"

echo "Created DMG: $OUTPUT_DMG"
