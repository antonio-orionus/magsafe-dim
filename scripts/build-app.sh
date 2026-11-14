#!/bin/sh
# Builds target/MagSafe Dim.app: the SwiftUI window plus the Rust daemon.
#
#   scripts/build-app.sh                       ad-hoc signature (works on this Mac only)
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" scripts/build-app.sh
#   SIGN_IDENTITY=... NOTARY_PROFILE=magsafe-dim scripts/build-app.sh
#       also notarizes and staples it, and writes target/MagSafe-Dim.zip
#       for download. NOTARY_PROFILE names credentials saved with
#       `xcrun notarytool store-credentials`.
#   OUT_DIR=target/dev scripts/build-app.sh    put the app somewhere else (default: target;
#                                              relative to the repository root)
#
# Run it as yourself, not with sudo, so codesign can use the key in your login keychain.
set -eu
cd "$(dirname "$0")/.."

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
out="${OUT_DIR:-target}"
app="$out/MagSafe Dim.app"
identity="${SIGN_IDENTITY:--}"

# The app is arm64 only (MagSafe 3 Macs are Apple silicon), so build the helper for it.
cargo build --release --locked --target aarch64-apple-darwin

rm -rf "${app:?}"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Library/LaunchDaemons"

swiftc -O -parse-as-library -swift-version 5 -target arm64-apple-macos13.0 \
  -o "$app/Contents/MacOS/MagSafe Dim" app/Main.swift app/MagSafeDim.swift
cp target/aarch64-apple-darwin/release/magsafe-dim "$app/Contents/MacOS/magsafe-dim"
sed "s/@VERSION@/$version/g" app/Info.plist > "$app/Contents/Info.plist"
cp app/dev.orionus.magsafe-dim.daemon.plist "$app/Contents/Library/LaunchDaemons/"
cp assets/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
plutil -lint -s "$app/Contents/Info.plist" "$app/Contents/Library/LaunchDaemons/dev.orionus.magsafe-dim.daemon.plist"

# Sign inside out: the daemon first, then the app, which seals everything in it.
if [ "$identity" = "-" ]; then
  set -- --sign -
else
  set -- --sign "$identity" --options runtime --timestamp
fi
# Apple's timestamp server sometimes doesn't answer: try three times.
sign() {
  for attempt in 1 2 3; do
    codesign --force "$@" && return 0
    echo "codesign attempt $attempt failed; retrying in 5 s" >&2
    sleep 5
  done
  return 1
}
sign "$@" --identifier dev.orionus.magsafe-dim.daemon "$app/Contents/MacOS/magsafe-dim"
sign "$@" "$app"
codesign --verify --strict --deep "$app"

team="$(codesign -dv "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
case "$team" in '' | 'not set') team= ;; esac
echo "built $app $version (signed: ${team:-ad-hoc})"

[ -n "${NOTARY_PROFILE:-}" ] || exit 0
[ -n "$team" ] || { echo "notarization needs a Developer ID signature (SIGN_IDENTITY)" >&2; exit 1; }

# Apple checks the app and issues a ticket; stapling attaches it, so Gatekeeper
# accepts a downloaded copy even offline.
zip="$out/MagSafe-Dim.zip"
rm -f "$zip"
ditto -c -k --norsrc --noextattr --keepParent "$app" "$zip"
# Keep going on failure, so Apple's explanation gets printed below.
xcrun notarytool submit "$zip" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json \
  > "$out/notarize.json" || true
status="$(plutil -extract status raw "$out/notarize.json" 2>/dev/null || echo "not submitted")"
if [ "$status" != "Accepted" ]; then
  echo "notarization: $status" >&2
  if id="$(plutil -extract id raw "$out/notarize.json" 2>/dev/null)"; then
    xcrun notarytool log "$id" --keychain-profile "$NOTARY_PROFILE" >&2 || true
  else
    cat "$out/notarize.json" >&2
  fi
  exit 1
fi
xcrun stapler staple "$app"
xcrun stapler validate "$app"
rm -f "$zip"
ditto -c -k --norsrc --noextattr --keepParent "$app" "$zip"
spctl --assess --type exec --verbose=2 "$app"
echo "notarized and stapled; download: $zip"
