#!/usr/bin/env bash
# Builds dist/Zapive-<version>-x86_64.AppImage from target/release/zapive.
# Run from the repository root (the release workflow does).
set -euo pipefail

VERSION="${1:?usage: make-appimage.sh <version>}"
APP_ID=io.github.wilssola.Zapive
APPDIR=build/AppDir

rm -rf "$APPDIR"
install -Dm755 target/release/zapive "$APPDIR/usr/bin/zapive"
install -Dm644 "packaging/linux/$APP_ID.desktop" "$APPDIR/usr/share/applications/$APP_ID.desktop"
install -Dm644 "packaging/linux/$APP_ID.metainfo.xml" "$APPDIR/usr/share/metainfo/$APP_ID.metainfo.xml"
install -Dm644 ui/zapive.png "$APPDIR/usr/share/icons/hicolor/1024x1024/apps/$APP_ID.png"

# appimagetool wants the desktop file and icon at the AppDir root too.
cp "packaging/linux/$APP_ID.desktop" "$APPDIR/"
cp ui/zapive.png "$APPDIR/$APP_ID.png"
ln -sf "$APP_ID.png" "$APPDIR/.DirIcon"
ln -sf usr/bin/zapive "$APPDIR/AppRun"

if [ ! -x build/appimagetool ]; then
    curl -fsSL -o build/appimagetool \
        https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x build/appimagetool
fi

mkdir -p dist
# --appimage-extract-and-run: CI runners have no FUSE for the tool itself.
ARCH=x86_64 build/appimagetool --appimage-extract-and-run --no-appstream \
    "$APPDIR" "dist/Zapive-$VERSION-x86_64.AppImage"
