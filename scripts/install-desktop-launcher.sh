#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
DESKTOP_DIR="$DATA_HOME/applications"
ICON_DIR="$DATA_HOME/icons/hicolor/scalable/apps"
DESKTOP_TEMPLATE="$ROOT_DIR/assets/linux/audetic.desktop"
DESKTOP_TARGET="$DESKTOP_DIR/audetic.desktop"

mkdir -p "$DESKTOP_DIR" "$ICON_DIR"

if [[ -x "$ROOT_DIR/target/release/audetic-launch" ]]; then
    EXEC_PATH="$ROOT_DIR/target/release/audetic-launch"
elif [[ -x "$ROOT_DIR/target/debug/audetic-launch" ]]; then
    EXEC_PATH="$ROOT_DIR/target/debug/audetic-launch"
elif command -v audetic-launch >/dev/null 2>&1; then
    EXEC_PATH="$(command -v audetic-launch)"
else
    EXEC_PATH="audetic-launch"
fi

awk -v exec_path="$EXEC_PATH" '
    BEGIN { replaced = 0 }
    /^Exec=/ {
        print "Exec=" exec_path
        replaced = 1
        next
    }
    { print }
    END {
        if (!replaced) {
            print "Exec=" exec_path
        }
    }
' "$DESKTOP_TEMPLATE" > "$DESKTOP_TARGET"
install -m 644 "$ROOT_DIR/assets/audetic_icon_light.svg" "$ICON_DIR/audetic.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$DESKTOP_DIR" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q "$DATA_HOME/icons/hicolor" >/dev/null 2>&1 || true
fi

if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 >/dev/null 2>&1 || true
elif command -v kbuildsycoca5 >/dev/null 2>&1; then
    kbuildsycoca5 >/dev/null 2>&1 || true
fi

echo "Installed desktop launcher: $DESKTOP_TARGET"
echo "Launcher command: $EXEC_PATH"
echo "Installed icon: $ICON_DIR/audetic.svg"
