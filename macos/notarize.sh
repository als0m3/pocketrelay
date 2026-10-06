#!/usr/bin/env bash
# Explicit release operation. Requires a Developer ID build and a saved notarytool profile.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${MACOS_NOTARY_PROFILE:?Set MACOS_NOTARY_PROFILE (notarytool profile in Keychain)}"
app="$PWD/dist/macos/PocketRelay.app"
if ! codesign -dv "$app" 2>&1 | grep -q 'Authority=Developer ID Application:'; then
  echo "Rebuild with MACOS_SIGN_IDENTITY='Developer ID Application: …' before notarizing."
  exit 1
fi
archive="$PWD/dist/macos/PocketRelay-notarization.zip"
ditto -c -k --keepParent "$app" "$archive"
xcrun notarytool submit "$archive" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
xcrun stapler staple "$app"
xcrun stapler validate "$app"
spctl --assess --type execute --verbose "$app"
# Recreate the distributable from the stapled app, not the earlier unsigned DMG.
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
ditto "$app" "$stage/PocketRelay.app"
ln -s /Applications "$stage/Applications"
printf 'Drag PocketRelay to Applications, then open it.\nAPI: http://localhost:8788/v1\n' > "$stage/Read me.txt"
dmg="$PWD/dist/macos/PocketRelay-$(uname -m).dmg"
hdiutil create -volname PocketRelay -srcfolder "$stage" -ov -format UDZO "$dmg"
xcrun notarytool submit "$dmg" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
