import Foundation

public enum IPCError: Error, CustomStringConvertible {
    case notRunning(String)
    case sendFailed
    case receiveFailed
    case decodeFailed

    public var description: String {
        switch self {
        case .notRunning(let path):
            return "rewindd is not running (\(path))"
        case .sendFailed: return "failed to send request to rewindd"
        case .receiveFailed: return "failed to read response from rewindd"
        case .decodeFailed: return "rewindd sent a response this client cannot decode"
        }
    }
}

/// Blocking client, used by `rewindctl`. The GPUI UI speaks the same protocol.
public struct IPCClient {
    public let path: String
    public let timeout: TimeInterval

    public init(path: String = SocketPath.default, timeout: TimeInterval = 60) {
        self.path = path
        self.timeout = timeout
    }

    public func isRunning() -> Bool {
        guard let fd = UnixSocket.connect(path) else { return false }
        close(fd)
        return true
    }

    public func send(_ request: Request) throws -> Reply {
        guard let fd = UnixSocket.connect(path) else {
            throw IPCError.notRunning(path)
        }
        defer { close(fd) }

        var timeoutValue = timeval(
            tv_sec: Int(timeout),
            tv_usec: suseconds_t((timeout - Double(Int(timeout))) * 1_000_000)
        )
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeoutValue, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeoutValue, socklen_t(MemoryLayout<timeval>.size))

        let payload = try JSONEncoder().encode(request)
        SocketIO.writeAll(fd, Frame.encode(payload))

        var decoder = FrameDecoder()
        while true {
            guard let chunk = SocketIO.readUpTo(fd, 64 * 1024) else {
                throw IPCError.receiveFailed
            }
            let frames = try decoder.append(chunk)
            guard let frame = frames.first else { continue }
            do {
                return try Reply.decode(frame)
            } catch {
                throw IPCError.decodeFailed
            }
        }
    }
}
