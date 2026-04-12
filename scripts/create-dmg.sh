#!/bin/bash
set -euo pipefail

# Packages a signed .app bundle into a styled DMG with an Applications symlink.
# Uses `create-dmg` (brew install create-dmg) for window styling.
# Usage: ./scripts/create-dmg.sh <app_bundle_path> <output_dmg_path>
# Example: ./scripts/create-dmg.sh target/release/Prvw.app Prvw_0.4.0_aarch64.dmg

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

if ! command -v create-dmg &>/dev/null; then
    echo "Error: create-dmg not found. Install it with: brew install create-dmg"
    exit 1
fi

# Remove existing DMG if present (create-dmg refuses to overwrite)
rm -f "$OUTPUT_DMG"

APP_NAME=$(basename "$APP_BUNDLE")

echo "Creating DMG: $OUTPUT_DMG..."
create-dmg \
    --volname "Prvw" \
    --window-size 600 400 \
    --icon-size 100 \
    --icon "$APP_NAME" 175 200 \
    --app-drop-link 425 200 \
    --no-internet-enable \
    --hide-extension "$APP_NAME" \
    "$OUTPUT_DMG" \
    "$APP_BUNDLE"

echo "Created DMG: $OUTPUT_DMG"
