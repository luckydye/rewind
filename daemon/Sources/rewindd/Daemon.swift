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
        let launcher = UILauncher()
        let path = SocketPath.default
        let server = IPCServer(path: path) { request in
            await Router.handle(request, recorder: recorder, launcher: launcher)
        }
        try server.start()

        let delegate = DaemonDelegate(server: server)
        Retain.delegate = delegate

        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        app.delegate = delegate

        let menu = MenuController(recorder: recorder, launcher: launcher)
        Retain.menu = menu
        menu.install()

        let terminate = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
        terminate.setEventHandler { NSApp.terminate(nil) }
        terminate.resume()
        signal(SIGTERM, SIG_IGN)

        HotKeyCenter.shared.action = { launcher.open() }
        if !HotKeyCenter.shared.register() {
            FileHandle.standardError.write(Data("rewindd: running without a hotkey\n".utf8))
        }

        // Preload the overlay hidden so the first hotkey is as fast as the rest.
        launcher.preload()

        FileHandle.standardError.write(Data("rewindd: listening on \(path)\n".utf8))
        Task { await recorder.start() }
        app.run()
    }
}
