#!/bin/bash
set -euo pipefail

# Packages a signed .app bundle into a DMG with an Applications symlink.
# Tries create-dmg for styling, falls back to plain hdiutil if Finder isn't available.
# Usage: ./scripts/create-dmg.sh <app_bundle_path> <output_dmg_path>

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

APP_NAME=$(basename "$APP_BUNDLE")

# Remove existing DMG if present
rm -f "$OUTPUT_DMG"

# Detach any stale Prvw* volumes left over from prior aborted runs or a manually
# mounted DMG. If they linger, macOS auto-renames our new volume to "Prvw 1" and
# TCC blocks the self-hosted runner from writing inside it ("Operation not permitted").
while IFS= read -r vol; do
    if [[ -n "$vol" ]]; then
        echo "Detaching stale mount: $vol"
        hdiutil detach "$vol" -force >/dev/null 2>&1 || true
    fi
done < <(mount | awk -F' on ' '/\/Volumes\/Prvw/ { sub(/ \(.*$/, "", $2); print $2 }')

# Try styled DMG with create-dmg (requires Finder, may fail in headless environments)
if command -v create-dmg &>/dev/null; then
    echo "Creating styled DMG with create-dmg..."
    if create-dmg \
        --volname "Prvw" \
        --window-size 600 400 \
        --icon-size 100 \
        --icon "$APP_NAME" 175 200 \
        --app-drop-link 425 200 \
        --no-internet-enable \
        --hide-extension "$APP_NAME" \
        "$OUTPUT_DMG" \
        "$APP_BUNDLE" 2>&1; then
        echo "Created styled DMG: $OUTPUT_DMG"
        exit 0
    else
        echo "create-dmg failed (Finder may not be available), falling back to hdiutil..."
        rm -f "$OUTPUT_DMG"
    fi
fi

# Fallback: plain hdiutil DMG
echo "Creating DMG with hdiutil..."
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

cp -R "$APP_BUNDLE" "$TEMP_DIR/"
ln -s /Applications "$TEMP_DIR/Applications"

hdiutil create \
    -volname "Prvw" \
    -srcfolder "$TEMP_DIR" \
    -ov \
    -format UDZO \
    "$OUTPUT_DMG"

echo "Created DMG: $OUTPUT_DMG"
