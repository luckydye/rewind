import CoreGraphics
import Foundation
import ImageIO
import RewindKit
import UniformTypeIdentifiers

enum StillImageError: Error, CustomStringConvertible {
    case imageCreationFailed
    case encodingFailed

    var description: String {
        switch self {
        case .imageCreationFailed: return "could not build an image from the frame"
        case .encodingFailed: return "could not encode the frame"
        }
    }
}

extension RawImage {
    /// PNG, encoded straight from a CGImage over the BGRA pixels. Going through
    /// NSBitmapImageRep also materialised a full uncompressed TIFF and left
    /// ~30 MB behind per copy.
    func pngData() throws -> Data {
        guard
            let provider = CGDataProvider(data: pixels as CFData),
            let cgImage = CGImage(
                width: width,
                height: height,
                bitsPerComponent: 8,
                bitsPerPixel: 32,
                bytesPerRow: width * 4,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGBitmapInfo(
                    rawValue: CGImageAlphaInfo.noneSkipFirst.rawValue
                        | CGBitmapInfo.byteOrder32Little.rawValue
                ),
                provider: provider,
                decode: nil,
                shouldInterpolate: false,
                intent: .defaultIntent
            )
        else {
            throw StillImageError.imageCreationFailed
        }

        let data = NSMutableData()
        guard
            let destination = CGImageDestinationCreateWithData(
                data as CFMutableData,
                UTType.png.identifier as CFString,
                1,
                nil
            )
        else {
            throw StillImageError.encodingFailed
        }
        CGImageDestinationAddImage(destination, cgImage, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw StillImageError.encodingFailed
        }
        return data as Data
    }
}
