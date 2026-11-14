#!/bin/sh
# Renders icon.svg into AppIcon.icns with macOS's own SVG support (no extra tools).
# Run after editing icon.svg; commit icon.svg, AppIcon.icns and icon.png.
set -eu
cd "$(dirname "$0")"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat > "$work/render.swift" <<'EOF'
import AppKit
let args = CommandLine.arguments
guard let image = NSImage(contentsOfFile: args[1]) else { fatalError("cannot read \(args[1])") }
let size = Int(args[3])!
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
                           bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                           colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
image.draw(in: NSRect(x: 0, y: 0, width: size, height: size))
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: args[2]))
EOF
swiftc -O -o "$work/render" "$work/render.swift"

set_dir="$work/AppIcon.iconset"
mkdir "$set_dir"
for s in 16 32 128 256 512; do
  "$work/render" icon.svg "$set_dir/icon_${s}x${s}.png" "$s"
  "$work/render" icon.svg "$set_dir/icon_${s}x${s}@2x.png" "$((s * 2))"
done
iconutil -c icns -o AppIcon.icns "$set_dir"
"$work/render" icon.svg icon.png 512
echo "wrote AppIcon.icns and icon.png"
