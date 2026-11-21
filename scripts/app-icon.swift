// Draws the Rewind app icon: a softly shaded #f97316 rounded square on the
// macOS 1024 icon grid. Regenerate `resources/Rewind.icns` with:
//   swiftc -O -o /tmp/draw-icon scripts/app-icon.swift
//   mkdir /tmp/Rewind.iconset && for n in 16 32 128 256 512; do /tmp/draw-icon /tmp/Rewind.iconset/icon_${n}x${n}.png $n;
//     /tmp/draw-icon /tmp/Rewind.iconset/icon_${n}x${n}@2x.png $((n*2)); done
//   iconutil -c icns /tmp/Rewind.iconset -o resources/Rewind.icns

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

// app-icon <out.png> <pixels>
let out = CommandLine.arguments[1]
let px = Int(CommandLine.arguments[2])!
let cs = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0, space: cs,
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
ctx.scaleBy(x: CGFloat(px) / 1024, y: CGFloat(px) / 1024)
func c(_ hex: UInt32, _ a: CGFloat = 1) -> CGColor {
    CGColor(colorSpace: cs, components: [CGFloat(hex >> 16 & 0xff) / 255, CGFloat(hex >> 8 & 0xff) / 255, CGFloat(hex & 0xff) / 255, a])!
}

let orange: UInt32 = 0xf97316
let body = CGRect(x: 100, y: 100, width: 824, height: 824)
let shape = CGPath(roundedRect: body, cornerWidth: 185, cornerHeight: 185, transform: nil)

// Drop shadow under the body.
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 24, color: c(0x000000, 0.35))
ctx.addPath(shape)
ctx.setFillColor(c(orange))
ctx.fillPath()
ctx.restoreGState()

// The orange, a little lighter at the top and deeper at the bottom.
ctx.saveGState()
ctx.addPath(shape)
ctx.clip()
let shading = CGGradient(colorsSpace: cs, colors: [c(0xfb923c), c(orange), c(0xea580c)] as CFArray, locations: [0, 0.5, 1])!
ctx.drawLinearGradient(shading, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 100), options: [])
// Hairline highlight along the edge.
ctx.addPath(CGPath(roundedRect: body.insetBy(dx: 1.5, dy: 1.5), cornerWidth: 184, cornerHeight: 184, transform: nil))
ctx.setStrokeColor(c(0xffffff, 0.18))
ctx.setLineWidth(3)
ctx.strokePath()
ctx.restoreGState()

let dest = CGImageDestinationCreateWithURL(URL(fileURLWithPath: out) as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(dest, ctx.makeImage()!, nil)
CGImageDestinationFinalize(dest)
