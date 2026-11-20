import Darwin
import Foundation

public enum IPCServerError: Error, CustomStringConvertible {
    case alreadyRunning(String)
    case socketFailed(Int32)
    case bindFailed(String, Int32)
    case listenFailed(Int32)

    public var description: String {
        switch self {
        case .alreadyRunning(let path):
            return "another rewindd is already listening on \(path)"
        case .socketFailed(let code):
            return "failed to create socket (errno \(code))"
        case .bindFailed(let path, let code):
            return "failed to bind \(path) (errno \(code))"
        case .listenFailed(let code):
            return "failed to listen (errno \(code))"
        }
    }
}

/// Unix-socket server. One request in, one response out — no subscriptions, so
/// a UI that dies mid-request cannot leave the daemon in a half-written state.
public final class IPCServer {
    public typealias Handler = @Sendable (Request) async -> Response

    private let path: String
    private let handler: Handler
    private let queue = DispatchQueue(label: "com.rewind.ipc.server")
    private var listenerFD: Int32 = -1
    private var acceptSource: DispatchSourceRead?
    private var connections: [Int32: IPCServer.Connection] = [:]

    public init(path: String, handler: @escaping Handler) {
        self.path = path
        self.handler = handler
    }

    public func start() throws {
        try SocketPath.ensureOwningDirectory(of: path)

        if IPCClient(path: path, timeout: 2).isRunning() {
            throw IPCServerError.alreadyRunning(path)
        }
        unlink(path)

        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw IPCServerError.socketFailed(errno) }

        var reuse: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &reuse, socklen_t(MemoryLayout<Int32>.size))

        var address = UnixSocket.makeAddress(path)
        let bound = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bound == 0 else {
            close(fd)
            throw IPCServerError.bindFailed(path, errno)
        }
        guard listen(fd, 8) == 0 else {
            close(fd)
            throw IPCServerError.listenFailed(errno)
        }
        chmod(path, 0o600)
        // The accept source only guarantees *a* connection is pending, not that
        // every subsequent accept() succeeds. A blocking listener would park the
        // server queue forever on the last iteration and deadlock shutdown.
        let listenerFlags = fcntl(fd, F_GETFL, 0)
        fcntl(fd, F_SETFL, listenerFlags | O_NONBLOCK)

        listenerFD = fd
        let source = DispatchSource.makeReadSource(fileDescriptor: fd, queue: queue)
        source.setEventHandler { [weak self] in self?.acceptConnections() }
        source.setCancelHandler { close(fd) }
        source.resume()
        acceptSource = source
    }

    public func stop() {
        queue.sync {
            acceptSource?.cancel()
            acceptSource = nil
            for connection in connections.values {
                connection.shutdown()
            }
            connections.removeAll()
            listenerFD = -1
        }
        unlink(path)
    }

    private func acceptConnections() {
        while true {
            let clientFD = accept(listenerFD, nil, nil)
            guard clientFD >= 0 else {
                if errno == EINTR { continue }
                return
            }

            // Connection reads are dispatched from a read source but performed
            // with blocking read(), so the flag must not be inherited.
            let clientFlags = fcntl(clientFD, F_GETFL, 0)
            fcntl(clientFD, F_SETFL, clientFlags & ~O_NONBLOCK)

            var noSigPipe: Int32 = 1
            setsockopt(
                clientFD,
                SOL_SOCKET,
                SO_NOSIGPIPE,
                &noSigPipe,
                socklen_t(MemoryLayout<Int32>.size)
            )

            let connection = Connection(fd: clientFD, handler: handler) { [weak self] closedFD in
                self?.queue.async {
                    self?.connections[closedFD] = nil
                }
            }
            connections[clientFD] = connection
            connection.start()
        }
    }
}

extension IPCServer {
    fileprivate final class Connection {
        private let fd: Int32
        private let handler: Handler
        private let onClose: @Sendable (Int32) -> Void
        private let queue: DispatchQueue
        private let writeQueue: DispatchQueue
        private var source: DispatchSourceRead?
        private var decoder = FrameDecoder()

        init(fd: Int32, handler: @escaping Handler, onClose: @escaping @Sendable (Int32) -> Void) {
            self.fd = fd
            self.handler = handler
            self.onClose = onClose
            self.queue = DispatchQueue(label: "com.rewind.ipc.connection.\(fd)")
            self.writeQueue = DispatchQueue(label: "com.rewind.ipc.write.\(fd)")
        }

        func start() {
            let source = DispatchSource.makeReadSource(fileDescriptor: fd, queue: queue)
            source.setEventHandler { [weak self] in self?.readAvailable() }
            source.setCancelHandler { [fd] in Darwin.close(fd) }
            source.resume()
            self.source = source
        }

        func shutdown() {
            source?.cancel()
            source = nil
        }

        private func readAvailable() {
            guard let chunk = SocketIO.readUpTo(fd, 64 * 1024) else {
                shutdown()
                onClose(fd)
                return
            }

            let frames: [Data]
            do {
                frames = try decoder.append(chunk)
            } catch {
                shutdown()
                onClose(fd)
                return
            }

            for frame in frames {
                guard let request = try? JSONDecoder().decode(Request.self, from: frame) else {
                    continue
                }
                Task { [handler] in
                    let response = await handler(request)
                    self.write(response)
                }
            }
        }

        private func write(_ response: Response) {
            guard let payload = try? JSONEncoder().encode(response) else { return }
            let framed = Frame.encode(payload)
            writeQueue.async { [fd] in
                SocketIO.writeAll(fd, framed)
            }
        }
    }
}
