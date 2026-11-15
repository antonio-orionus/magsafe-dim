#!/bin/sh
# Builds target/MagSafe Dim.app: the SwiftUI window plus the Rust daemon.
#
#   scripts/build-app.sh                       ad-hoc signature (works on this Mac only)
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" scripts/build-app.sh
#   SIGN_IDENTITY=... NOTARY_PROFILE=magsafe-dim scripts/build-app.sh
#       also notarizes and staples the app, then builds, signs, notarizes and
#       staples target/MagSafe-Dim.dmg for download (needs uv for dmgbuild).
#       NOTARY_PROFILE names credentials saved with `xcrun notarytool store-credentials`.
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

# Sends a file to Apple's notary service and waits for the verdict. Apple's
# service sometimes reports "offline" on a working connection, so try three times.
notarize() {
  for attempt in 1 2 3; do
    xcrun notarytool submit "$1" --keychain-profile "$NOTARY_PROFILE" --wait --output-format json \
      > "$out/notarize.json" 2>/dev/null || true
    status="$(plutil -extract status raw "$out/notarize.json" 2>/dev/null || echo "not submitted")"
    [ "$status" = "Accepted" ] && return 0
    # Apple looked at it and said no: retrying won't help. Show why.
    if id="$(plutil -extract id raw "$out/notarize.json" 2>/dev/null)"; then
      echo "notarization of $1: $status" >&2
      xcrun notarytool log "$id" --keychain-profile "$NOTARY_PROFILE" >&2 || true
      return 1
    fi
    echo "notarization attempt $attempt: not submitted; retrying in 10 s" >&2
    sleep 10
  done
  return 1
}

# 1. The app: Apple checks it and issues a ticket; stapling attaches the ticket, so
#    Gatekeeper accepts a copy even offline. Zipped without extended attributes:
#    their ._ files break the signature when unpacked by other tools.
zip="$out/MagSafe-Dim.zip"
rm -f "$zip"
ditto -c -k --norsrc --noextattr --keepParent "$app" "$zip"
notarize "$zip"
xcrun stapler staple "$app"
rm -f "$zip"
ditto -c -k --norsrc --noextattr --keepParent "$app" "$zip"
spctl --assess --type exec --verbose=2 "$app"

# 2. The download: a disk image with the stapled app and a link to Applications,
#    laid out by dmgbuild (scripts/dmg-settings.py), then signed, notarized, stapled.
dmg="$out/MagSafe-Dim.dmg"
rm -f "$dmg"
uvx --from 'dmgbuild==1.6.7' dmgbuild -s scripts/dmg-settings.py \
  -D app="$app" -D background=assets/dmg-background.tiff "MagSafe Dim" "$dmg"
sign --sign "$identity" --timestamp "$dmg"  # retried: the timestamp server sometimes doesn't answer
notarize "$dmg"
xcrun stapler staple "$dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
echo "notarized and stapled; download: $dmg"
