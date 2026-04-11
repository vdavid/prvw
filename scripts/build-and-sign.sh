#!/bin/bash
set -euo pipefail

# Build and codesign the Prvw macOS app.
# Usage: ./scripts/build-and-sign.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DESKTOP_DIR="$PROJECT_ROOT/apps/desktop"
BINARY_NAME="prvw"
BUNDLE_ID="com.veszelovszki.prvw"
SIGNING_IDENTITY="Developer ID Application: Rymdskottkarra AB (83H6YAQMNP)"
ENTITLEMENTS="$DESKTOP_DIR/Entitlements.plist"

echo "Building release binary..."
cd "$DESKTOP_DIR"
cargo build --release

BINARY="$DESKTOP_DIR/target/release/$BINARY_NAME"
if [ ! -f "$BINARY" ]; then
    echo "Build failed: binary not found at $BINARY"
    exit 1
fi

echo "Signing binary with hardened runtime..."
codesign \
    --sign "$SIGNING_IDENTITY" \
    --identifier "$BUNDLE_ID" \
    --options runtime \
    --entitlements "$ENTITLEMENTS" \
    --force \
    --verbose \
    "$BINARY"

echo ""
echo "Verifying signature..."
codesign --verify --verbose=2 "$BINARY"

echo ""
echo "Done! Signed binary at: $BINARY"
echo "  Bundle ID: $BUNDLE_ID"
echo "  Identity: $SIGNING_IDENTITY"
