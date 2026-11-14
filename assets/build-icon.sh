#!/bin/sh
# Renders the SVGs here with macOS's own SVG support (no extra tools):
#   icon.svg           -> AppIcon.icns (the app icon) and icon.png (README)
#   dmg-background.svg -> dmg-background.tiff (disk image window, 1x and Retina)
# Run after editing an SVG; commit the SVG and the files made from it.
set -eu
cd "$(dirname "$0")"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat > "$work/render.swift" <<'EOF'
import AppKit
let args = CommandLine.arguments
guard let image = NSImage(contentsOfFile: args[1]) else { fatalError("cannot read \(args[1])") }
let width = Int(args[3])!
let height = args.count > 4 ? Int(args[4])! : width
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height,
                           bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                           colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
image.draw(in: NSRect(x: 0, y: 0, width: width, height: height))
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

"$work/render" dmg-background.svg "$work/bg.png" 660 760
"$work/render" dmg-background.svg "$work/bg@2x.png" 1320 1520
tiffutil -cathidpicheck "$work/bg.png" "$work/bg@2x.png" -out dmg-background.tiff 2>/dev/null
echo "wrote AppIcon.icns, icon.png and dmg-background.tiff"
