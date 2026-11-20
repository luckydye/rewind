import Foundation

/// A decoded frame on its way to the UI: straight BGRA, no compression.
public struct RawImage {
    public var width: Int
    public var height: Int
    /// Tightly packed BGRA, `width * height * 4` bytes.
    public var pixels: Data

    public init(width: Int, height: Int, pixels: Data) {
        assert(pixels.count == width * height * 4, "pixels must be tightly packed BGRA")
        self.width = width
        self.height = height
        self.pixels = pixels
    }
}

public enum ReplyError: Error, CustomStringConvertible {
    case empty
    case unknownKind(UInt8)
    case truncated

    public var description: String {
        switch self {
        case .empty: return "empty reply"
        case .unknownKind(let kind): return "unknown reply kind \(kind)"
        case .truncated: return "reply payload is truncated"
        }
    }
}

/// Replies are either JSON or a raw image. Images skip JSON and base64
/// entirely: at native resolution a frame is several megabytes, and encoding
/// that as text costs more CPU than the memcpy that delivers it.
public enum Reply {
    case json(Response)
    case image(RawImage)
    /// A batch of images, so a timeline strip is one round trip instead of one
    /// per thumbnail.
    case images([RawImage])

    public static let jsonKind: UInt8 = 0
    public static let imageKind: UInt8 = 1
    public static let imagesKind: UInt8 = 2

    /// Writes straight to the socket. An image frame is several megabytes, so
    /// assembling it in an intermediate `Data` first would copy it twice more
    /// than necessary.
    public func writeAll(to fd: Int32) throws {
        switch self {
        case .json(let response):
            var payload = Data([Self.jsonKind])
            payload.append(try JSONEncoder().encode(response))
            SocketIO.writeAll(fd, Frame.encode(payload))

        case .images(let images):
            let body = images.reduce(0) { $0 + 8 + $1.pixels.count }
            var prefix = Data(capacity: 9)
            // kind + count + entries
            prefix.append(bigEndian: UInt32(1 + 4 + body))
            prefix.append(Self.imagesKind)
            prefix.append(bigEndian: UInt32(images.count))

            SocketIO.writeAll(fd, prefix)
            for image in images {
                var header = Data(capacity: 8)
                header.append(bigEndian: UInt32(image.width))
                header.append(bigEndian: UInt32(image.height))
                SocketIO.writeAll(fd, header)
                SocketIO.writeAll(fd, image.pixels)
            }

        case .image(let image):
            var prefix = Data(capacity: 13)
            prefix.append(bigEndian: UInt32(1 + 8 + image.pixels.count))
            prefix.append(Self.imageKind)
            prefix.append(bigEndian: UInt32(image.width))
            prefix.append(bigEndian: UInt32(image.height))

            SocketIO.writeAll(fd, prefix)
            SocketIO.writeAll(fd, image.pixels)
        }
    }

    public static func decode(_ payload: Data) throws -> Reply {
        guard let kind = payload.first else { throw ReplyError.empty }
        let body = payload.dropFirst()

        switch kind {
        case jsonKind:
            return .json(try JSONDecoder().decode(Response.self, from: Data(body)))

        case imageKind:
            guard body.count >= 8 else { throw ReplyError.truncated }
            let width = Int(body.readBigEndianUInt32(at: 0))
            let height = Int(body.readBigEndianUInt32(at: 4))
            let pixels = Data(body.dropFirst(8))
            guard pixels.count == width * height * 4 else { throw ReplyError.truncated }
            return .image(RawImage(width: width, height: height, pixels: pixels))

        case imagesKind:
            guard body.count >= 4 else { throw ReplyError.truncated }
            let count = Int(body.readBigEndianUInt32(at: 0))
            var images: [RawImage] = []
            var offset = 4
            for _ in 0..<count {
                guard body.count >= offset + 8 else { throw ReplyError.truncated }
                let width = Int(body.readBigEndianUInt32(at: offset))
                let height = Int(body.readBigEndianUInt32(at: offset + 4))
                offset += 8
                let size = width * height * 4
                guard body.count >= offset + size else { throw ReplyError.truncated }
                images.append(
                    RawImage(
                        width: width,
                        height: height,
                        pixels: Data(body.dropFirst(offset).prefix(size))
                    )
                )
                offset += size
            }
            return .images(images)

        default:
            throw ReplyError.unknownKind(kind)
        }
    }
}

extension Data {
    mutating func append(bigEndian value: UInt32) {
        var big = value.bigEndian
        Swift.withUnsafeBytes(of: &big) { append(contentsOf: $0) }
    }

    func readBigEndianUInt32(at offset: Int) -> UInt32 {
        withUnsafeBytes { raw in
            raw.loadUnaligned(fromByteOffset: offset, as: UInt32.self).bigEndian
        }
    }
}
