#!/usr/bin/env bash
# Builds dist/Puzil-<version>-<arch>.AppImage for this machine's architecture with
# linuxdeploy (fetched into dist/appimage-tools on first use). With PUZIL_UPDATE_KEY_FILE
# set to the ed25519 release key, also writes the .sig that Puzil checks before it installs
# an update. Used by the release workflow, and by hand.
set -euo pipefail
cd "$(dirname "$0")/../.."

VERSION=$(awk -F\" '/^version/ {print $2; exit}' Cargo.toml)
ARCH=$(uname -m)
OUT="dist/Puzil-$VERSION-$ARCH.AppImage"

cargo build --release -p puzil-app

TOOLS=dist/appimage-tools
LD="$TOOLS/linuxdeploy-$ARCH.AppImage"
mkdir -p "$TOOLS"
if [[ ! -x $LD ]]; then
  curl -fsSL -o "$LD" "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$ARCH.AppImage"
  chmod +x "$LD"
fi

STAGE=dist/appimage-stage
rm -rf dist/AppDir "$STAGE" "$OUT"
mkdir -p "$STAGE"
cp target/release/puzil-app "$STAGE/puzil"
sed "s|@BIN@|puzil|" tools/linux/com.satchlj.puzil.desktop.in > "$STAGE/com.satchlj.puzil.desktop"
cp assets/icon/puzil-512.png "$STAGE/com.satchlj.puzil.png"

# Extract-and-run: CI machines and some desktops have no FUSE.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$ARCH" LDAI_OUTPUT="$OUT" OUTPUT="$OUT" "$LD" \
  --appdir dist/AppDir \
  --executable "$STAGE/puzil" \
  --desktop-file "$STAGE/com.satchlj.puzil.desktop" \
  --icon-file "$STAGE/com.satchlj.puzil.png" \
  --output appimage
rm -rf dist/AppDir "$STAGE"

if [[ -n ${PUZIL_UPDATE_KEY_FILE:-} ]]; then
  openssl pkeyutl -sign -inkey "$PUZIL_UPDATE_KEY_FILE" -rawin -in "$OUT" -out "$OUT.sig"
fi
echo "$OUT"
