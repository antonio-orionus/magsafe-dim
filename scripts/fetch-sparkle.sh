#!/bin/sh
# Downloads Sparkle (the app's updater) into target/sparkle, once, checked
# against a pinned SHA-256. scripts/build-app.sh and CI run it.
#
#   target/sparkle/Sparkle.framework    linked into and copied into the app
#   target/sparkle/bin/                 generate_keys, sign_update (for releases)
#
# To move to a newer Sparkle: pick a release at least a week old, take its
# digest from `gh api repos/sparkle-project/Sparkle/releases/tags/<version>`,
# and update both lines below.
set -eu
cd "$(dirname "$0")/.."

version=2.10.1
sha256=1a856d2a9edb8e628de88121d7be04cf0e2167f8641650b78fca00b14244cf27

dir=target/sparkle
[ "$(cat "$dir/VERSION" 2>/dev/null)" = "$version" ] && exit 0

archive="target/Sparkle-$version.tar.xz"
mkdir -p target
if [ ! -f "$archive" ] || ! echo "$sha256  $archive" | shasum -a 256 -c - >/dev/null 2>&1; then
  curl --fail --location --silent --show-error --proto '=https' --output "$archive.part" \
    "https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-$version.tar.xz"
  mv "$archive.part" "$archive"
fi
echo "$sha256  $archive" | shasum -a 256 -c - >/dev/null || {
  echo "Sparkle $version: SHA-256 mismatch, refusing to use $archive" >&2
  rm -f "$archive"
  exit 1
}

rm -rf "$dir"
mkdir -p "$dir"
tar -xJf "$archive" -C "$dir" Sparkle.framework bin
echo "$version" > "$dir/VERSION"
echo "Sparkle $version in $dir"
