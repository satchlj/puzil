#!/usr/bin/env bash
# Builds Puzil and installs it for the current user: the binary in ~/.local/bin, a desktop
# entry (which also registers Puzil for the zulip:// links that finish an SSO sign-in) and
# the icon. PREFIX=/usr/local installs system-wide instead (run as root).
set -euo pipefail
cd "$(dirname "$0")/../.."

PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin/puzil"
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor/512x512/apps"

cargo build --release -p puzil-app

install -Dm755 target/release/puzil-app "$BIN"
install -Dm644 assets/icon/puzil-512.png "$ICONS/com.satchlj.puzil.png"
mkdir -p "$APPS"
sed "s|@BIN@|$BIN|" tools/linux/com.satchlj.puzil.desktop.in > "$APPS/com.satchlj.puzil.desktop"

command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" || true
command -v xdg-mime >/dev/null && xdg-mime default com.satchlj.puzil.desktop x-scheme-handler/zulip || true

echo "Installed $BIN"
