#!/usr/bin/env bash
# Package the Linux sela binary as tar.gz + AppImage.
# Env:
#   VERSION   - version string without leading v (required)
#   BINARY    - path to the release binary (default: target/release/sela)
#   OUT_DIR   - output directory (default: dist)
#   REPO_ROOT - repository root (default: cwd)
#   ARCH      - architecture label (default: x86_64)
set -euo pipefail

VERSION="${VERSION:?VERSION required}"
BINARY="${BINARY:-target/release/sela}"
OUT_DIR="${OUT_DIR:-dist}"
REPO_ROOT="${REPO_ROOT:-$(pwd)}"
ARCH="${ARCH:-x86_64}"

mkdir -p "$OUT_DIR"
test -x "$BINARY"

base="sela-${VERSION}-linux-${ARCH}"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

mkdir -p "$stage/$base"
cp "$BINARY" "$stage/$base/sela"
cp "$REPO_ROOT/LICENSE" "$REPO_ROOT/README.md" "$stage/$base/"
(
  cd "$stage"
  tar -czf "$OLDPWD/$OUT_DIR/${base}.tar.gz" "$base"
)

# Minimal AppDir: do not bundle Vulkan/Wayland/X11 — the host provides those.
# GPUI loads the GPU stack at runtime from the system.
appdir="$stage/AppDir"
mkdir -p "$appdir/usr/bin" \
  "$appdir/usr/share/applications" \
  "$appdir/usr/share/icons/hicolor/256x256/apps"
cp "$BINARY" "$appdir/usr/bin/sela"
cp "$REPO_ROOT/packaging/linux/sela.desktop" "$appdir/usr/share/applications/"
cp "$REPO_ROOT/packaging/linux/sela.png" "$appdir/usr/share/icons/hicolor/256x256/apps/"
ln -sf usr/bin/sela "$appdir/AppRun"
ln -sf usr/share/applications/sela.desktop "$appdir/sela.desktop"
ln -sf usr/share/icons/hicolor/256x256/apps/sela.png "$appdir/sela.png"

tools="$stage/tools"
mkdir -p "$tools"
curl -fsSL -o "$tools/appimagetool" \
  "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${ARCH}.AppImage"
chmod +x "$tools/appimagetool"

# GitHub-hosted runners often lack usable FUSE; extract-and-run avoids mounting.
export APPIMAGE_EXTRACT_AND_RUN=1
export ARCH
"$tools/appimagetool" "$appdir" "$OUT_DIR/Sela-${VERSION}-${ARCH}.AppImage"
chmod +x "$OUT_DIR/Sela-${VERSION}-${ARCH}.AppImage"

# Smoke the AppImage headlessly (no FUSE required with extract-and-run).
APPIMAGE_EXTRACT_AND_RUN=1 "$OUT_DIR/Sela-${VERSION}-${ARCH}.AppImage" --version \
  | grep -E "^sela ${VERSION}( |$)"

ls -lh "$OUT_DIR/${base}.tar.gz" "$OUT_DIR/Sela-${VERSION}-${ARCH}.AppImage"
