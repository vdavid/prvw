#!/bin/bash
set -euo pipefail

# Build the Windows installer. Works from macOS, Linux, or Windows: `makensis` cross-compiles,
# and on a Mac `cargo-xwin` supplies the exe. See docs/guides/releasing.md.
#
# Usage: ./scripts/build-windows-installer.sh [--exe <path>]
#
#   --exe <path>  Package this prvw.exe instead of cross-building one. That's how a Windows
#                 release runner hands over the binary its own cargo just produced.
#
# Operators (humans and AI agents): NEVER tail, head, or filter this script's output.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DESKTOP_DIR="$PROJECT_ROOT/apps/desktop"
INSTALLER_DIR="$DESKTOP_DIR/installer/windows"
TARGET="x86_64-pc-windows-msvc"
OUT_DIR="$PROJECT_ROOT/target/windows-installer"

EXE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --exe)
      EXE="${2:-}"
      if [[ -z "$EXE" ]]; then
        echo "Error: --exe needs a path."
        exit 1
      fi
      shift 2
      ;;
    -h | --help)
      sed -n '4,12p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "Error: unknown argument $1"
      exit 1
      ;;
  esac
done

if ! command -v makensis >/dev/null 2>&1; then
  echo "Error: makensis isn't installed."
  echo "  macOS:   brew install makensis"
  echo "  Linux:   apt install nsis"
  echo "  Windows: winget install NSIS.NSIS"
  exit 1
fi

# One source of truth for the version: the crate's own. The exe's VERSIONINFO comes from the same
# place, through build.rs and CARGO_PKG_VERSION, so the two can't disagree.
VERSION=$(grep '^version' "$DESKTOP_DIR/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Error: couldn't read a X.Y.Z version out of apps/desktop/Cargo.toml (got '$VERSION')."
  exit 1
fi
echo "Version: $VERSION"

# The registry writes the installer makes are generated from the app's own `file_types` module.
# Regenerating here means a build can't ship a stale list even if someone skipped the check.
echo "Generating the file-type registration..."
CARGO_TARGET_DIR="$PROJECT_ROOT/target/xtask" \
  cargo run --quiet --manifest-path "$PROJECT_ROOT/Cargo.toml" --package xtask -- installer-registry \
  >"$INSTALLER_DIR/file-associations.nsh"

if [[ -z "$EXE" ]]; then
  echo "Cross-building prvw.exe for $TARGET..."
  if ! command -v cargo-xwin >/dev/null 2>&1; then
    echo "Error: cargo-xwin isn't installed. Run 'cargo install cargo-xwin --locked'."
    exit 1
  fi
  # cargo-xwin ships clang-cl and lld-link but no MSVC archiver. The windows-cross check links
  # rustup's llvm-ar in as `llvm-lib`; run it once if that hasn't happened yet.
  if [[ ! -e "$PROJECT_ROOT/target/cross-check-bin/llvm-lib" ]]; then
    echo "Error: target/cross-check-bin/llvm-lib is missing."
    echo "Run './scripts/check.sh --check windows-cross' once to create it."
    exit 1
  fi
  PATH="$PROJECT_ROOT/target/cross-check-bin:$PATH" \
    cargo xwin build --release --target "$TARGET" --manifest-path "$PROJECT_ROOT/Cargo.toml" -p prvw
  EXE="$PROJECT_ROOT/target/$TARGET/release/prvw.exe"
fi

if [[ ! -f "$EXE" ]]; then
  echo "Error: no prvw.exe at $EXE"
  exit 1
fi
EXE="$(cd "$(dirname "$EXE")" && pwd)/$(basename "$EXE")"

# Signing slots in here. `PRVW_WINDOWS_SIGN_CMD` is run once per file, with the file as its only
# argument, and it has to sign in place. Nothing in this repo holds a credential; the release
# workflow is where Azure Trusted Signing supplies one. See docs/guides/releasing.md.
sign_if_configured() {
  local file="$1"
  if [[ -n "${PRVW_WINDOWS_SIGN_CMD:-}" ]]; then
    echo "Signing $(basename "$file")..."
    "$PRVW_WINDOWS_SIGN_CMD" "$file"
  fi
}

sign_if_configured "$EXE"

# On a Windows host this script runs under Git Bash, which hands paths around as `/d/a/...`.
# `makensis` is a native Windows program and reads none of that, so translate with `cygpath`,
# which only exists on the shells that need it. Everywhere else the path is already the right one.
# The bash-side variables stay POSIX, because `[[ -f ]]` and `du` want them that way.
#
# EVERY path handed to makensis goes through this, with no separator glued on afterwards. makensis
# splits a path on the host separator alone, so a half-translated one breaks `!include` on Windows.
# See the comment beside the defines in `prvw.nsi`.
to_native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s' "$1"
  fi
}

mkdir -p "$OUT_DIR"
OUTFILE="$OUT_DIR/PrvwSetup-$VERSION-x64.exe"
rm -f "$OUTFILE"

echo "Packaging $OUTFILE..."
makensis \
  -INPUTCHARSET UTF8 \
  -DPRVW_VERSION="$VERSION" \
  -DPRVW_EXE="$(to_native_path "$EXE")" \
  -DPRVW_OUTFILE="$(to_native_path "$OUTFILE")" \
  -DPRVW_INSTALLER_DIR="$(to_native_path "$INSTALLER_DIR")" \
  -DPRVW_LICENSE="$(to_native_path "$PROJECT_ROOT/LICENSE")" \
  -DPRVW_ICON="$(to_native_path "$DESKTOP_DIR/resources/AppIcon.ico")" \
  "$(to_native_path "$INSTALLER_DIR/prvw.nsi")"

if [[ ! -f "$OUTFILE" ]]; then
  echo "Error: makensis reported success but wrote no installer."
  exit 1
fi

sign_if_configured "$OUTFILE"

SIZE=$(du -h "$OUTFILE" | cut -f1)
echo ""
echo "Built $OUTFILE ($SIZE)"
