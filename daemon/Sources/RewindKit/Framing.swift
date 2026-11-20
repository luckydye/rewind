import Foundation

public enum FramingError: Error, CustomStringConvertible {
    case frameTooLarge(Int)

    public var description: String {
        switch self {
        case .frameTooLarge(let size):
            return "IPC frame of \(size) bytes exceeds the 64 MiB limit"
        }
    }
}

/// Length-prefixed framing: big-endian UInt32 byte count followed by the
/// payload. Trivial to mirror in Rust.
public enum Frame {
    /// Generous: a native-resolution BGRA frame of a 4K display is ~33 MiB.
    public static let maximumSize = 64 * 1024 * 1024

    public static func encode(_ payload: Data) -> Data {
        var framed = Data(capacity: payload.count + 4)
        var length = UInt32(payload.count).bigEndian
        withUnsafeBytes(of: &length) { framed.append(contentsOf: $0) }
        framed.append(payload)
        return framed
    }
}

public struct FrameDecoder {
    private var buffer = Data()

    public init() {}

    public mutating func append(_ data: Data) throws -> [Data] {
        buffer.append(data)
        var frames: [Data] = []

        while buffer.count >= 4 {
            let length = buffer.withUnsafeBytes {
                $0.loadUnaligned(fromByteOffset: 0, as: UInt32.self).bigEndian
            }
            guard length <= Frame.maximumSize else {
                throw FramingError.frameTooLarge(Int(length))
            }
            guard buffer.count >= 4 + Int(length) else {
                buffer.reserveCapacity(4 + Int(length))
                break
            }

            let start = buffer.startIndex + 4
            frames.append(buffer.subdata(in: start..<start + Int(length)))
            buffer.removeSubrange(buffer.startIndex..<start + Int(length))
        }

        return frames
    }
}

public enum SocketIO {
    public static func writeAll(_ fd: Int32, _ data: Data) {
        data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            var offset = 0
            while offset < raw.count {
                let written = write(fd, base.advanced(by: offset), raw.count - offset)
                if written > 0 {
                    offset += written
                } else if written < 0 && errno == EINTR {
                    continue
                } else {
                    return
                }
            }
        }
    }

    /// Reads exactly `count` bytes. Used for reply bodies, which can be several
    /// megabytes: accumulating them in chunks costs more than the transfer.
    public static func readExactly(_ fd: Int32, _ count: Int) -> Data? {
        var data = Data(count: count)
        let complete = data.withUnsafeMutableBytes { raw -> Bool in
            guard let base = raw.baseAddress else { return count == 0 }
            var offset = 0
            while offset < count {
                let bytes = read(fd, base + offset, count - offset)
                if bytes > 0 {
                    offset += bytes
                } else if bytes < 0 && errno == EINTR {
                    continue
                } else {
                    return false
                }
            }
            return true
        }
        return complete ? data : nil
    }

    public static func readUpTo(_ fd: Int32, _ count: Int) -> Data? {
        var buffer = [UInt8](repeating: 0, count: count)
        let bytes = buffer.withUnsafeMutableBytes { read(fd, $0.baseAddress, count) }
        if bytes > 0 {
            return Data(buffer[0..<bytes])
        }
        if bytes < 0 && errno == EINTR {
            return readUpTo(fd, count)
        }
        return nil
    }
}
