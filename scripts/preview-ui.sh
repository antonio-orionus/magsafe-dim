#!/bin/sh
# Renders the app's window in every state, dark and light, to target/ui-preview/*.png.
# No signing, no install, no daemon: for checking the design.
set -eu
cd "$(dirname "$0")/.."
mkdir -p target/ui-preview
swiftc -O -parse-as-library -swift-version 5 -target arm64-apple-macos13.0 \
  -o target/ui-preview/render app/MagSafeDim.swift app/Preview.swift
target/ui-preview/render target/ui-preview assets/AppIcon.icns
