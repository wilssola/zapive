#!/usr/bin/env bash
# Installs a Zapive AppImage for the current user, so it shows up in the
# desktop's app search and menu like any installed app:
#   ~/.local/bin/Zapive.AppImage
#   ~/.local/share/applications/io.github.wilssola.Zapive.desktop
#   ~/.local/share/icons/hicolor/256x256/apps/io.github.wilssola.Zapive.png
# Usage: install-desktop.sh Zapive-<version>-x86_64.AppImage
#        install-desktop.sh --uninstall
# Nothing needs root; the conversations (~/.local/share/zapive) are never
# touched, not even by --uninstall.
set -euo pipefail

APP_ID=io.github.wilssola.Zapive
BIN_DIR="$HOME/.local/bin"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
TARGET="$BIN_DIR/Zapive.AppImage"

refresh_menus() {
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$APP_DIR" || true
    command -v gtk-update-icon-cache >/dev/null &&
        gtk-update-icon-cache -q -t "${ICON_DIR%/256x256/apps}" 2>/dev/null || true
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$TARGET" "$APP_DIR/$APP_ID.desktop" "$ICON_DIR/$APP_ID.png"
    refresh_menus
    echo "Zapive removed from the app menu. Conversations were kept."
    exit 0
fi

SOURCE="${1:?usage: install-desktop.sh <Zapive.AppImage> | --uninstall}"
[ -f "$SOURCE" ] || { echo "not found: $SOURCE" >&2; exit 1; }

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"
install -m755 "$SOURCE" "$TARGET"

# The icon ships inside the AppImage; extracting needs no FUSE.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
if (cd "$tmp" && "$TARGET" --appimage-extract "$APP_ID.png" >/dev/null 2>&1) &&
    [ -f "$tmp/squashfs-root/$APP_ID.png" ]; then
    install -m644 "$tmp/squashfs-root/$APP_ID.png" "$ICON_DIR/$APP_ID.png"
fi

# Exec is an absolute path: a launcher does not see the shell's PATH.
cat >"$APP_DIR/$APP_ID.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Zapive
Comment=Native WhatsApp desktop client
Comment[pt_BR]=Cliente WhatsApp nativo para desktop
Exec="$TARGET"
Icon=$APP_ID
Terminal=false
Categories=Network;InstantMessaging;Chat;
Keywords=whatsapp;chat;messaging;
StartupWMClass=zapive
EOF
refresh_menus

echo "Installed: search for \"Zapive\" in your apps."
# What the AppImage needs at run time but Ubuntu may not have.
missing=()
ldconfig -p 2>/dev/null | grep -q 'libfuse\.so\.2' || missing+=(libfuse2t64)
command -v secret-tool >/dev/null || missing+=(libsecret-tools)
if [ ${#missing[@]} -gt 0 ]; then
    echo "Before the first start, install: sudo apt install ${missing[*]}"
fi
