#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
DESKTOP_DIR="$DATA_HOME/applications"
ICON_DIR="$DATA_HOME/icons/hicolor/scalable/apps"

mkdir -p "$DESKTOP_DIR" "$ICON_DIR"

install -m 644 "$ROOT_DIR/assets/linux/audetic.desktop" "$DESKTOP_DIR/audetic.desktop"
install -m 644 "$ROOT_DIR/assets/audetic_icon_light.svg" "$ICON_DIR/audetic.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$DESKTOP_DIR" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q "$DATA_HOME/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed desktop launcher: $DESKTOP_DIR/audetic.desktop"
echo "Installed icon: $ICON_DIR/audetic.svg"
