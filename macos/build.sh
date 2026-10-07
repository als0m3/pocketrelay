#!/usr/bin/env bash
# Build on the target Mac architecture. No build tools are needed by the app's users.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)" == Darwin ]] || { echo "This build requires macOS."; exit 1; }
for tool in cargo python3 xcrun codesign; do
  command -v "$tool" >/dev/null || { echo "$tool is required to build the app."; exit 1; }
done
arch="$(uname -m)"
app="$PWD/dist/macos/PocketRelay.app"
# This directory only contains generated artifacts, never user data.
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/bin" "$app/Contents/Frameworks"
# Remap compiler paths so panic locations and metadata do not reveal the build machine.
export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS:+${CARGO_ENCODED_RUSTFLAGS}$'\x1f'}--remap-path-prefix=$HOME=/build-home"$'\x1f'"--remap-path-prefix=$PWD=/src/pocketrelay"
cargo build --release --locked
cp target/release/customremote "$app/Contents/Resources/bin/"
cp macos/Info.plist "$app/Contents/Info.plist"
cp -R static "$app/Contents/Resources/"
mkdir -p "$app/Contents/Resources/ThirdParty"
cp src/provider-tools.json "$app/Contents/Resources/ThirdParty/provider-tools.json"
cp macos/THIRD-PARTY.md "$app/Contents/Resources/ThirdParty/README.md"
cp LICENSE "$app/Contents/Resources/LICENSE.txt"
cp -R macos/notices/. "$app/Contents/Resources/ThirdParty/"
xcrun swiftc -swift-version 5 -O -target "${arch}-apple-macosx14.0" \
  -file-prefix-map "$HOME=/build-home" -file-prefix-map "$PWD=/src/pocketrelay" \
  -debug-prefix-map "$HOME=/build-home" -debug-prefix-map "$PWD=/src/pocketrelay" \
  -framework AppKit -framework WebKit -framework ServiceManagement \
  macos/Sources/main.swift -o "$app/Contents/MacOS/PocketRelay"
xcrun swiftc -swift-version 5 -O -target "${arch}-apple-macosx14.0" -framework PDFKit -framework AppKit \
  -file-prefix-map "$HOME=/build-home" -file-prefix-map "$PWD=/src/pocketrelay" \
  -debug-prefix-map "$HOME=/build-home" -debug-prefix-map "$PWD=/src/pocketrelay" \
  macos/Sources/PDFTool.swift -o "$app/Contents/Resources/bin/pdfinfo"
cp "$app/Contents/Resources/bin/pdfinfo" "$app/Contents/Resources/bin/pdftotext"
cp "$app/Contents/Resources/bin/pdfinfo" "$app/Contents/Resources/bin/pdftoppm"
iconset="$PWD/dist/macos/AppIcon.iconset"
mkdir -p "$iconset"
xcrun swift macos/scripts/icon.swift "$iconset"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
# Drop download provenance and Finder metadata before signing and packaging.
xattr -cr "$app"
identity="${MACOS_SIGN_IDENTITY:--}"
sign_options=()
if [[ "$identity" != - ]]; then sign_options=(--options runtime --timestamp); fi
# Sign inside out. Only the Claude runtime needs the JIT entitlement.
while IFS= read -r -d '' file; do
  codesign --force --sign "$identity" ${sign_options[@]+"${sign_options[@]}"} "$file"
done < <(find "$app/Contents/Frameworks" -type f -name '*.dylib' -print0)
for file in "$app/Contents/Resources/bin/"*; do
  if [[ "${file##*/}" == claude ]]; then
    codesign --force --sign "$identity" ${sign_options[@]+"${sign_options[@]}"} --entitlements macos/cli-entitlements.plist "$file"
  else
    codesign --force --sign "$identity" ${sign_options[@]+"${sign_options[@]}"} "$file"
  fi
done
codesign --force --sign "$identity" ${sign_options[@]+"${sign_options[@]}"} "$app"
codesign --verify --deep --strict "$app"
python3 macos/scripts/verify.py "$app"
python3 macos/scripts/check-privacy.py "$app"
if [[ "${MACOS_SKIP_DMG:-0}" != 1 ]]; then
  stage="$(mktemp -d)"
  trap 'rm -rf "$stage"' EXIT
  ditto "$app" "$stage/PocketRelay.app"
  ln -s /Applications "$stage/Applications"
  cp macos/INSTALLER.txt "$stage/Read me.txt"
  hdiutil create -volname PocketRelay -srcfolder "$stage" -ov -format UDZO "$PWD/dist/macos/PocketRelay-${arch}.dmg"
fi
printf '\nApplication ready: %s\n' "$app"
if [[ "$identity" == - ]]; then
  echo "Local build without notarization. For public distribution, see docs/macos.md."
fi
