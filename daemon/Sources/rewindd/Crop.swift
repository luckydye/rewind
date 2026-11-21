import Foundation
import RewindKit

enum CropError: Error, CustomStringConvertible {
    case empty
    case outOfBounds

    var description: String {
        switch self {
        case .empty: return "the crop is empty"
        case .outOfBounds: return "the crop falls outside the capture"
        }
    }
}

extension Crop {
    /// The crop as whole pixels of a `width` by `height` frame. Even dimensions
    /// keep the H.264 encoder happy; the rectangle is shrunk to fit if needed.
    func pixels(width: Int, height: Int) -> CGRect {
        let x = (self.x * Double(width)).rounded()
        let y = (self.y * Double(height)).rounded()
        var w = (self.width * Double(width)).rounded()
        var h = (self.height * Double(height)).rounded()
        w -= w.truncatingRemainder(dividingBy: 2)
        h -= h.truncatingRemainder(dividingBy: 2)
        return CGRect(x: x, y: y, width: max(2, min(w, Double(width) - x)), height: max(2, min(h, Double(height) - y)))
    }
}

extension RawImage {
    /// The part of the frame the crop keeps. Frames are BGRA and tightly packed.
    func cropped(to crop: Crop) throws -> RawImage {
        let rect = crop.pixels(width: width, height: height)
        let x = Int(rect.origin.x)
        let y = Int(rect.origin.y)
        let w = Int(rect.width)
        let h = Int(rect.height)
        guard x >= 0, y >= 0, x + w <= width, y + h <= height else {
            throw CropError.outOfBounds
        }
        guard w > 0, h > 0 else { throw CropError.empty }

        var pixels = Data(count: w * h * 4)
        self.pixels.withUnsafeBytes { source in
            pixels.withUnsafeMutableBytes { destination in
                guard let source = source.baseAddress, let destination = destination.baseAddress else {
                    return
                }
                for row in 0..<h {
                    memcpy(
                        destination + row * w * 4,
                        source + ((y + row) * width + x) * 4,
                        w * 4
                    )
                }
            }
        }
        return RawImage(width: w, height: h, pixels: pixels)
    }
}
