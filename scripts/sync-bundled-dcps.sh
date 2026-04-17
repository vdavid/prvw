#!/usr/bin/env bash
# Sync the bundled DCP collection from RawTherapee's dev branch.
#
# Downloads all .dcp files from:
#   https://github.com/Beep6581/RawTherapee/tree/dev/rtdata/dcpprofiles
#
# Into: apps/desktop/build-assets/dcps/
#
# Usage:
#   ./scripts/sync-bundled-dcps.sh        # sync all (skip existing)
#   ./scripts/sync-bundled-dcps.sh --all  # re-download everything
#
# After running, commit the changed .dcp files and rebuild.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="$REPO_ROOT/apps/desktop/build-assets/dcps"
API_URL='https://api.github.com/repositories/40539005/contents/rtdata/dcpprofiles?per_page=200'
BASE_RAW='https://raw.githubusercontent.com/Beep6581/RawTherapee/dev/rtdata/dcpprofiles'

FORCE_ALL=false
if [[ "${1:-}" == "--all" ]]; then
    FORCE_ALL=true
fi

mkdir -p "$OUT_DIR"

echo "Fetching DCP file list from GitHub..."
NAMES=$(curl -sSfL "$API_URL" | python3 -c "
import json, sys, urllib.parse
data = json.load(sys.stdin)
for item in data:
    if item['name'].lower().endswith('.dcp'):
        print(item['name'])
" | sort)

OK=0
SKIP=0
FAIL=0

while IFS= read -r NAME; do
    DEST="$OUT_DIR/$NAME"
    if [[ "$FORCE_ALL" == false && -f "$DEST" ]]; then
        SKIP=$((SKIP + 1))
        continue
    fi
    ENCODED=$(python3 -c "import urllib.parse; print(urllib.parse.quote('$NAME'))")
    URL="$BASE_RAW/$ENCODED"
    if curl -sSfL -o "$DEST" "$URL"; then
        OK=$((OK + 1))
    else
        echo "FAIL: $NAME"
        FAIL=$((FAIL + 1))
    fi
done <<< "$NAMES"

TOTAL=$(echo "$NAMES" | wc -l | tr -d ' ')
echo ""
echo "Done: $TOTAL total, $OK downloaded, $SKIP skipped (already present), $FAIL failed"
if [[ $FAIL -gt 0 ]]; then
    echo "Some downloads failed. Check your connection and retry."
    exit 1
fi
echo ""
echo "Next steps:"
echo "  1. Review changes: git diff --stat apps/desktop/build-assets/dcps/"
echo "  2. Commit: git add apps/desktop/build-assets/dcps/ && git commit -m 'Assets: sync bundled DCP collection from RT'"
echo "  3. Rebuild: cd apps/desktop && cargo build --release"
