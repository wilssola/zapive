#!/usr/bin/env bash
# Builds Zapive.app (proper bundle: icon, no Terminal window on launch)
# and packages it as dist/Zapive-<version>.dmg with a drag-to-Applications
# layout. Run from the repository root (the release workflow does).
set -euo pipefail

VERSION="${1:?usage: make-bundle.sh <version> [signing-identity]}"
# "-" is the ad-hoc identity; a real one keeps the signature (and with it
# the app's TCC permissions) stable across releases.
IDENTITY="${2:--}"
APP=build/Zapive.app

rm -rf "$APP" build/zapive.iconset build/dmg
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" dist

cp target/release/zapive "$APP/Contents/MacOS/zapive"
sed "s/__VERSION__/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"

# .icns straight from the 1024px master.
ICONSET=build/zapive.iconset
mkdir -p "$ICONSET"
for size in 16 32 64 128 256 512; do
    sips -z "$size" "$size" ui/zapive.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" ui/zapive.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/zapive.icns"

# Signature: Apple Silicon refuses to run unsigned bundles at all.
# Ad-hoc signatures carry no certificate, so they can't be timestamped.
TIMESTAMP=(--timestamp)
[ "$IDENTITY" = "-" ] && TIMESTAMP=()
codesign --force --deep "${TIMESTAMP[@]}" --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict "$APP"

STAGE=build/dmg
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Zapive" -srcfolder "$STAGE" -ov -format UDZO "dist/Zapive-$VERSION.dmg"

if [ "$IDENTITY" != "-" ]; then
    codesign --force --timestamp --sign "$IDENTITY" "dist/Zapive-$VERSION.dmg"
fi

