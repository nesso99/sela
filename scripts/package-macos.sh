#!/usr/bin/env bash
# Package a macOS sela binary as .app inside a .dmg and a .zip.
# Env:
#   VERSION     - version without leading v (required)
#   BINARY      - path to the binary (required)
#   OUT_DIR     - output directory (default: dist)
#   REPO_ROOT   - repository root (default: cwd)
#   ARCH_LABEL  - artifact arch label (default: arm64)
set -euo pipefail

VERSION="${VERSION:?VERSION required}"
BINARY="${BINARY:?BINARY required}"
OUT_DIR="${OUT_DIR:-dist}"
REPO_ROOT="${REPO_ROOT:-$(pwd)}"
ARCH_LABEL="${ARCH_LABEL:-arm64}"

mkdir -p "$OUT_DIR"
test -x "$BINARY"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

app="$stage/Sela.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$BINARY" "$app/Contents/MacOS/sela"
chmod +x "$app/Contents/MacOS/sela"
# Substitute version into Info.plist
sed "s/@VERSION@/${VERSION}/g" "$REPO_ROOT/packaging/macos/Info.plist" \
  >"$app/Contents/Info.plist"
cp "$REPO_ROOT/LICENSE" "$app/Contents/Resources/"

base="sela-${VERSION}-macos-${ARCH_LABEL}"

# Zip of the .app (handy when Gatekeeper blocks the DMG open-in-place flow)
(
  cd "$stage"
  zip -qy -r "$OLDPWD/$OUT_DIR/${base}.zip" Sela.app
)

# Unsigned DMG via hdiutil
dmg_src="$stage/dmg"
mkdir -p "$dmg_src"
cp -R "$app" "$dmg_src/"
ln -s /Applications "$dmg_src/Applications"
hdiutil create \
  -volname "Sela ${VERSION}" \
  -srcfolder "$dmg_src" \
  -ov -format UDZO \
  "$OUT_DIR/${base}.dmg"

ls -lh "$OUT_DIR/${base}.zip" "$OUT_DIR/${base}.dmg"
