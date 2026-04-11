#!/bin/bash
set -euo pipefail

# Build, bundle, and codesign the Prvw macOS app.
# Usage: ./scripts/build-and-sign.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DESKTOP_DIR="$PROJECT_ROOT/apps/desktop"
BINARY_NAME="prvw"
BUNDLE_ID="com.veszelovszki.prvw"
SIGNING_IDENTITY="Developer ID Application: Rymdskottkarra AB (83H6YAQMNP)"
ENTITLEMENTS="$DESKTOP_DIR/Entitlements.plist"
INFO_PLIST="$DESKTOP_DIR/Info.plist"
ICON_PATH="$DESKTOP_DIR/resources/AppIcon.icns"

# Extract version from Cargo.toml
VERSION=$(grep '^version' "$DESKTOP_DIR/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
echo "Version: $VERSION"

echo "Building release binary..."
cd "$DESKTOP_DIR"
cargo build --release

BINARY="$DESKTOP_DIR/target/release/$BINARY_NAME"
if [[ ! -f "$BINARY" ]]; then
    echo "Build failed: binary not found at $BINARY"
    exit 1
fi

# Create .app bundle
APP_BUNDLE="$DESKTOP_DIR/target/release/Prvw.app"
rm -rf "$APP_BUNDLE"

echo "Creating .app bundle..."
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

cp "$BINARY" "$APP_BUNDLE/Contents/MacOS/prvw"
chmod +x "$APP_BUNDLE/Contents/MacOS/prvw"

sed "s/__VERSION__/$VERSION/g" "$INFO_PLIST" > "$APP_BUNDLE/Contents/Info.plist"

cp "$ICON_PATH" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"

echo "Signing .app bundle with hardened runtime..."
codesign \
    --sign "$SIGNING_IDENTITY" \
    --identifier "$BUNDLE_ID" \
    --options runtime \
    --entitlements "$ENTITLEMENTS" \
    --force \
    --verbose \
    --deep \
    "$APP_BUNDLE"

echo ""
echo "Verifying signature..."
codesign --verify --verbose=2 "$APP_BUNDLE"

echo ""
echo "Bundle structure:"
find "$APP_BUNDLE" -type f | sort

echo ""
echo "Done! Signed app bundle at: $APP_BUNDLE"
echo "  Version: $VERSION"
echo "  Bundle ID: $BUNDLE_ID"
echo "  Identity: $SIGNING_IDENTITY"
