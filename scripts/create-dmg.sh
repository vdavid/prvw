#!/bin/bash
set -euo pipefail

# Packages a signed .app bundle into a DMG with an Applications symlink.
# Usage: ./scripts/create-dmg.sh <app_bundle_path> <output_dmg_path>
# Example: ./scripts/create-dmg.sh target/release/Prvw.app Prvw_0.3.0_aarch64.dmg

APP_BUNDLE="${1:-}"
OUTPUT_DMG="${2:-}"

if [[ -z "$APP_BUNDLE" || -z "$OUTPUT_DMG" ]]; then
    echo "Usage: ./scripts/create-dmg.sh <app_bundle_path> <output_dmg_path>"
    exit 1
fi

if [[ ! -d "$APP_BUNDLE" ]]; then
    echo "Error: .app bundle not found at $APP_BUNDLE"
    exit 1
fi

APP_NAME=$(basename "$APP_BUNDLE" .app)

TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

# Copy the signed .app bundle into the staging directory
cp -R "$APP_BUNDLE" "$TEMP_DIR/"

# Create Applications symlink for drag-to-install
ln -s /Applications "$TEMP_DIR/Applications"

# Remove existing DMG if present
rm -f "$OUTPUT_DMG"

echo "Creating DMG: $OUTPUT_DMG..."
hdiutil create \
    -volname "$APP_NAME" \
    -srcfolder "$TEMP_DIR" \
    -ov \
    -format UDZO \
    "$OUTPUT_DMG"

echo "Created DMG: $OUTPUT_DMG"
