import AppKit
import CoreGraphics
import Foundation
import RewindKit

enum ClipboardError: Error, CustomStringConvertible {
    case imageCreationFailed
    case encodingFailed
    case writeFailed

    var description: String {
        switch self {
        case .imageCreationFailed: return "could not build an image from the frame"
        case .encodingFailed: return "could not encode the frame"
        case .writeFailed: return "the pasteboard refused the frame"
        }
    }
}

enum Clipboard {
    /// Puts a decoded frame on the general pasteboard as PNG and TIFF. PNG is
    /// what browsers and chat apps take; TIFF is what Preview and older AppKit
    /// apps paste.
    @MainActor
    static func copy(_ image: RawImage) throws {
        guard
            let provider = CGDataProvider(data: image.pixels as CFData),
            let cgImage = CGImage(
                width: image.width,
                height: image.height,
                bitsPerComponent: 8,
                bitsPerPixel: 32,
                bytesPerRow: image.width * 4,
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
            throw ClipboardError.imageCreationFailed
        }

        let representation = NSBitmapImageRep(cgImage: cgImage)
        guard
            let png = representation.representation(using: .png, properties: [:]),
            let tiff = representation.tiffRepresentation
        else {
            throw ClipboardError.encodingFailed
        }

        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        guard pasteboard.setData(png, forType: .png), pasteboard.setData(tiff, forType: .tiff) else {
            throw ClipboardError.writeFailed
        }
    }
}
