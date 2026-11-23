#!/bin/zsh
# Builds dist/Puzil.app from a release build.
#   tools/macos/bundle.sh            ad-hoc signed (runs here; Gatekeeper warns elsewhere)
#   PUZIL_SIGN_ID="Developer ID Application: …" tools/macos/bundle.sh
#                                    signed with hardened runtime, ready to notarize
#   PUZIL_NOTARY_PROFILE=<keychain profile> also notarizes and staples (notarytool)
set -e
cd "${0:A:h}/../.."
VERSION=$(awk -F\" '/^version/ {print $2; exit}' Cargo.toml)
BUILD=$(git rev-list --count HEAD)
cargo build --release -p puzil-app
APP=dist/Puzil.app
rm -rf $APP
mkdir -p $APP/Contents/MacOS $APP/Contents/Resources
cp target/release/puzil-app $APP/Contents/MacOS/puzil
cp assets/icon/Puzil.icns $APP/Contents/Resources/
sed -e "s/@VERSION@/$VERSION/" -e "s/@BUILD@/$BUILD/" tools/macos/Info.plist.in > $APP/Contents/Info.plist
if [[ -n "$PUZIL_SIGN_ID" ]]; then
  codesign --force --options runtime --timestamp --sign "$PUZIL_SIGN_ID" $APP
else
  codesign --force --sign - $APP
fi
codesign --verify --deep --strict $APP
if [[ -n "$PUZIL_NOTARY_PROFILE" && -n "$PUZIL_SIGN_ID" ]]; then
  ditto -c -k --keepParent $APP dist/Puzil.zip
  xcrun notarytool submit dist/Puzil.zip --keychain-profile "$PUZIL_NOTARY_PROFILE" --wait
  xcrun stapler staple $APP
fi
ditto -c -k --keepParent $APP dist/Puzil-$VERSION.zip
echo "$APP  (version $VERSION, build $BUILD)"
