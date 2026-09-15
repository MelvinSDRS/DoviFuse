#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
APP="$ROOT/dist/DV8 Maker.app"
DMG="$ROOT/dist/DV8-Maker-arm64.dmg"
STAGING="$ROOT/.macos-build-cache/dmg-root"

if [[ ! -d $APP ]]; then
  echo "Missing app bundle: $APP" >&2
  echo "Run macapp/build-macos-app.sh first." >&2
  exit 1
fi

codesign --verify --deep --strict "$APP"

rm -rf "$STAGING"
mkdir -p "$STAGING"
ditto "$APP" "$STAGING/DV8 Maker.app"
ln -s /Applications "$STAGING/Applications"

rm -f "$DMG" "$DMG.sha256"
hdiutil create \
  -volname "DV8 Maker" \
  -srcfolder "$STAGING" \
  -format UDZO \
  -imagekey zlib-level=9 \
  -ov \
  "$DMG"
hdiutil verify "$DMG"
(cd "$ROOT/dist" && shasum -a 256 "$(basename "$DMG")" > "$(basename "$DMG").sha256")

echo "Built: $DMG"
