import AppKit
import Foundation
import RewindKit

private enum Retain {
    static var delegate: DaemonDelegate?
    static var menu: MenuController?
}

final class DaemonDelegate: NSObject, NSApplicationDelegate {
    private let server: IPCServer

    init(server: IPCServer) {
        self.server = server
    }

    func applicationWillTerminate(_ notification: Notification) {
        server.stop()
    }
}

enum Daemon {
    static func run() throws {
        // A client that dies mid-response must not take the daemon with it.
        signal(SIGPIPE, SIG_IGN)

        let recorder = Recorder(config: ConfigStore.load())
        let path = SocketPath.default
        let server = IPCServer(path: path) { request in
            await Router.handle(request, recorder: recorder)
        }
        try server.start()

        let delegate = DaemonDelegate(server: server)
        Retain.delegate = delegate

        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        app.delegate = delegate

        let menu = MenuController(recorder: recorder)
        Retain.menu = menu
        menu.install()

        let terminate = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
        terminate.setEventHandler { NSApp.terminate(nil) }
        terminate.resume()
        signal(SIGTERM, SIG_IGN)

        FileHandle.standardError.write(Data("rewindd: listening on \(path)\n".utf8))
        Task { await recorder.start() }
        app.run()
    }
}
