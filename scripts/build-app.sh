#!/bin/sh
# Builds target/MagSafe Dim.app: the SwiftUI window plus the Rust daemon.
#
#   scripts/build-app.sh                       ad-hoc signature (works on this Mac only)
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" scripts/build-app.sh
#   SIGN_IDENTITY=... NOTARY_PROFILE=magsafe-dim scripts/build-app.sh
#       also notarizes and staples the app, then builds, signs, notarizes and
#       staples target/MagSafe-Dim.dmg for download (needs uv for dmgbuild), and
#       writes target/appcast.xml for Sparkle, signed with its EdDSA key.
#       RELEASE_NOTES=<file.md> puts the notes in the update window.
#       NOTARY_PROFILE names credentials saved with `xcrun notarytool store-credentials`.
#   OUT_DIR=target/dev scripts/build-app.sh    put the app somewhere else (default: target;
#                                              relative to the repository root)
#
# Keys: from SIGN_KEYCHAIN (default ~/Library/Keychains/signing.keychain-db) when it
# exists, a keychain that locks itself after a few idle minutes: the Developer ID
# identity, usable by codesign without asking, and the Sparkle key as the generic
# password magsafe-dim-sparkle. Without it, both come from the login keychain, which
# asks at every use. Run it as yourself, not with sudo, so it can use your keychains.
set -eu
cd "$(dirname "$0")/.."

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
out="${OUT_DIR:-target}"
app="$out/MagSafe Dim.app"
identity="${SIGN_IDENTITY:--}"
keychain="${SIGN_KEYCHAIN:-$HOME/Library/Keychains/signing.keychain-db}"
[ -f "$keychain" ] || keychain=

# The app is arm64 only (MagSafe 3 Macs are Apple silicon), so build the helper for it.
cargo build --release --locked --target aarch64-apple-darwin
scripts/fetch-sparkle.sh

rm -rf "${app:?}"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Library/LaunchDaemons" \
  "$app/Contents/Frameworks"

swiftc -O -parse-as-library -swift-version 5 -target arm64-apple-macos13.0 \
  -F target/sparkle -framework Sparkle -Xlinker -rpath -Xlinker @executable_path/../Frameworks \
  -o "$app/Contents/MacOS/MagSafe Dim" app/Main.swift app/MagSafeDim.swift app/Updater.swift
cp target/aarch64-apple-darwin/release/magsafe-dim "$app/Contents/MacOS/magsafe-dim"
# Sparkle's XPC services are for sandboxed apps only (this app isn't), and its
# headers only for compiling.
sparkle="$app/Contents/Frameworks/Sparkle.framework"
ditto target/sparkle/Sparkle.framework "$sparkle"
for unused in XPCServices Headers PrivateHeaders Modules; do
  rm -rf "${sparkle:?}/Versions/B/$unused" "${sparkle:?}/$unused"
done
sed "s/@VERSION@/$version/g" app/Info.plist > "$app/Contents/Info.plist"
cp app/dev.orionus.magsafe-dim.daemon.plist "$app/Contents/Library/LaunchDaemons/"
cp assets/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
plutil -lint -s "$app/Contents/Info.plist" "$app/Contents/Library/LaunchDaemons/dev.orionus.magsafe-dim.daemon.plist"

# Sign inside out: Sparkle's helpers, Sparkle, the daemon, then the app, which seals
# everything in it.
if [ "$identity" = "-" ]; then
  set -- --sign -
else
  set -- --sign "$identity" --options runtime --timestamp
fi
# Apple's timestamp server sometimes doesn't answer: try three times. Not when the
# key was refused (a dialog cancelled or timed out), which would only ask again.
sign() {
  if [ "$identity" != "-" ] && [ -n "$keychain" ]; then
    set -- --keychain "$keychain" "$@"
  fi
  for attempt in 1 2 3; do
    if result="$(codesign --force "$@" 2>&1)"; then
      [ -z "$result" ] || printf '%s\n' "$result"
      return 0
    fi
    printf '%s\n' "$result" >&2
    case "$result" in
      *errSecInternalComponent* | *"User interaction is not allowed"* | *"canceled"*)
        echo "codesign: the signing key was refused; not retrying" >&2
        return 1
        ;;
    esac
    echo "codesign attempt $attempt failed; retrying in 5 s" >&2
    sleep 5
  done
  return 1
}
sign "$@" "$sparkle/Versions/B/Autoupdate"
sign "$@" "$sparkle/Versions/B/Updater.app"
sign "$@" "$sparkle"
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

# Sparkle's feed: one item, the zip above, which the release must carry as
# MagSafe-Dim.zip next to appcast.xml, both signed with the EdDSA key: piped from
# the signing keychain, or read by sign_update from the login keychain.
sparkle_sign() {
  if [ -n "$keychain" ] &&
    security find-generic-password -s magsafe-dim-sparkle -a magsafe-dim "$keychain" >/dev/null 2>&1; then
    security find-generic-password -s magsafe-dim-sparkle -a magsafe-dim -w "$keychain" |
      target/sparkle/bin/sign_update --ed-key-file - "$@"
  else
    target/sparkle/bin/sign_update --account magsafe-dim "$@"
  fi
}
repo=https://github.com/antonio-orionus/magsafe-dim
enclosure="$(sparkle_sign "$zip")"
notes=
if [ -n "${RELEASE_NOTES:-}" ]; then
  if grep -q ']]>' "$RELEASE_NOTES"; then
    echo "$RELEASE_NOTES contains ]]>, which would end the feed's CDATA" >&2
    exit 1
  fi
  notes="      <description sparkle:format=\"markdown\"><![CDATA[
$(cat "$RELEASE_NOTES")
]]></description>"
fi
appcast="$out/appcast.xml"
cat > "$appcast" <<FEED
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>MagSafe Dim</title>
    <item>
      <title>MagSafe Dim $version</title>
      <pubDate>$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S +0000')</pubDate>
      <sparkle:version>$version</sparkle:version>
      <sparkle:shortVersionString>$version</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>13.0</sparkle:minimumSystemVersion>
      <sparkle:fullReleaseNotesLink>$repo/releases/tag/v$version</sparkle:fullReleaseNotesLink>
$notes
      <enclosure url="$repo/releases/download/v$version/MagSafe-Dim.zip" type="application/octet-stream" $enclosure/>
    </item>
  </channel>
</rss>
FEED
xmllint --noout "$appcast"
sparkle_sign "$appcast"
sparkle_sign --verify "$appcast"

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
echo "notarized and stapled; release assets: $dmg $zip $appcast"
