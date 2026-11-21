import CoreGraphics
import Foundation
import RewindKit

enum AnnotationError: Error, CustomStringConvertible {
    case unreadable(String)
    case malformed
    case sizeMismatch(annotation: String, frame: String)
    case imageCreationFailed

    var description: String {
        switch self {
        case .unreadable(let path): return "cannot read annotations at \(path)"
        case .malformed: return "the annotation layer is malformed"
        case .sizeMismatch(let annotation, let frame):
            return "annotations are \(annotation) but the frame is \(frame)"
        case .imageCreationFailed: return "could not build an image from the annotations"
        }
    }
}

/// The drawing the overlay sends alongside a copy or export.
///
/// The file is `u32 width`, `u32 height` (big-endian), then straight-alpha
/// RGBA. Keeping it out of the JSON request avoids base64 on a layer that can
/// be tens of megabytes at native resolution.
struct AnnotationOverlay {
    let width: Int
    let height: Int
    /// Straight-alpha RGBA, sRGB.
    let pixels: Data

    static func load(_ path: String) throws -> AnnotationOverlay {
        guard let data = FileManager.default.contents(atPath: path) else {
            throw AnnotationError.unreadable(path)
        }
        guard data.count >= 8 else { throw AnnotationError.malformed }
        let width = Int(Self.bigEndianUInt32(data, 0))
        let height = Int(Self.bigEndianUInt32(data, 4))
        let size = width * height * 4
        guard width > 0, height > 0, data.count == 8 + size else {
            throw AnnotationError.malformed
        }
        return AnnotationOverlay(
            width: width,
            height: height,
            pixels: Data(data.dropFirst(8))
        )
    }

    private static func bigEndianUInt32(_ data: Data, _ offset: Int) -> UInt32 {
        data.withUnsafeBytes {
            $0.loadUnaligned(fromByteOffset: offset, as: UInt32.self).bigEndian
        }
    }

    /// Source-over onto a decoded frame. The frame is BGRA; the annotations are
    /// RGBA, so the two red and blue channels are swapped on the way in.
    func composite(_ base: RawImage) throws -> RawImage {
        guard base.width == width, base.height == height else {
            throw AnnotationError.sizeMismatch(
                annotation: "\(width)×\(height)",
                frame: "\(base.width)×\(base.height)"
            )
        }

        let count = width * height
        var output = base.pixels
        pixels.withUnsafeBytes { source in
            output.withUnsafeMutableBytes { destination in
                guard let source = source.baseAddress, let destination = destination.baseAddress else {
                    return
                }
                let sourceBytes = source.assumingMemoryBound(to: UInt8.self)
                let destinationBytes = destination.assumingMemoryBound(to: UInt8.self)
                for pixel in 0..<count {
                    let at = pixel * 4
                    let alpha = Double(sourceBytes[at + 3]) / 255.0
                    if alpha <= 0.0 { continue }
                    let keep = 1.0 - alpha
                    let red = Double(sourceBytes[at])
                    let green = Double(sourceBytes[at + 1])
                    let blue = Double(sourceBytes[at + 2])
                    destinationBytes[at] = UInt8((blue * alpha + Double(destinationBytes[at]) * keep).rounded())
                    destinationBytes[at + 1] = UInt8((green * alpha + Double(destinationBytes[at + 1]) * keep).rounded())
                    destinationBytes[at + 2] = UInt8((red * alpha + Double(destinationBytes[at + 2]) * keep).rounded())
                    destinationBytes[at + 3] = 255
                }
            }
        }
        return RawImage(width: width, height: height, pixels: output)
    }

    /// The overlay as a Core Graphics image for the video composition.
    func cgImage() throws -> CGImage {
        var premultiplied = Data(count: pixels.count)
        pixels.withUnsafeBytes { source in
            premultiplied.withUnsafeMutableBytes { destination in
                guard let source = source.baseAddress, let destination = destination.baseAddress else {
                    return
                }
                let sourceBytes = source.assumingMemoryBound(to: UInt8.self)
                let destinationBytes = destination.assumingMemoryBound(to: UInt8.self)
                for index in stride(from: 0, to: pixels.count, by: 4) {
                    let alpha = Double(sourceBytes[index + 3]) / 255.0
                    destinationBytes[index] = UInt8((Double(sourceBytes[index]) * alpha).rounded())
                    destinationBytes[index + 1] = UInt8((Double(sourceBytes[index + 1]) * alpha).rounded())
                    destinationBytes[index + 2] = UInt8((Double(sourceBytes[index + 2]) * alpha).rounded())
                    destinationBytes[index + 3] = sourceBytes[index + 3]
                }
            }
        }

        guard let provider = CGDataProvider(data: premultiplied as CFData) else {
            throw AnnotationError.imageCreationFailed
        }
        guard let image = CGImage(
            width: width,
            height: height,
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(
                rawValue: CGImageAlphaInfo.premultipliedLast.rawValue
                    | CGBitmapInfo.byteOrder32Big.rawValue
            ),
            provider: provider,
            decode: nil,
            shouldInterpolate: true,
            intent: .defaultIntent
        ) else {
            throw AnnotationError.imageCreationFailed
        }
        return image
    }
}
