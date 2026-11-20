import AppKit
import Foundation
import RewindKit

/// Owns the overlay process and the hotkey hand-off.
///
/// The overlay is preloaded hidden and outlives its uses, so pressing the
/// hotkey only has to show a window that already exists: the process launch,
/// Metal setup and window creation all happen at login instead of on every
/// press.
final class UILauncher {
    private var process: Process?

    /// Only used for the fallback spawn, where the overlay has to be told to
    /// show itself once it is up.
    private static let activationAttempts = 40

    static var socketPath: String {
        URL(fileURLWithPath: SocketPath.default)
            .deletingLastPathComponent()
            .appendingPathComponent("ui.sock")
            .path
    }

    /// Runs at daemon start. Harmless when an overlay is already listening.
    func preload() {
        if poke() { return }
        spawn(arguments: ["--resident"])
    }

    func open() {
        if poke() { return }
        // Nothing is listening: the overlay was never preloaded or it died.
        spawn(arguments: ["--show"])
    }

    /// One byte down the overlay's socket is the whole show request.
    private func poke() -> Bool {
        guard let fd = UnixSocket.connect(Self.socketPath) else { return false }
        defer { close(fd) }
        var byte: UInt8 = 1
        return write(fd, &byte, 1) == 1
    }

    private func spawn(arguments: [String]) {
        guard let executable = Bundle.main.executableURL else {
            FileHandle.standardError.write(Data("rewindd: no executable URL\n".utf8))
            return
        }
        let url = executable.deletingLastPathComponent().appendingPathComponent("rewind-ui")
        guard FileManager.default.isExecutableFile(atPath: url.path) else {
            FileHandle.standardError.write(
                Data("rewindd: cannot find rewind-ui at \(url.path)\n".utf8)
            )
            return
        }

        let process = Process()
        process.executableURL = url
        process.arguments = arguments
        process.terminationHandler = { [weak self] _ in
            self?.process = nil
        }
        do {
            try process.run()
            self.process = process
            if arguments.contains("--show") {
                activate(pid: process.processIdentifier, attempts: Self.activationAttempts)
            }
        } catch {
            FileHandle.standardError.write(
                Data("rewindd: cannot launch rewind-ui: \(error)\n".utf8)
            )
        }
    }

    /// `NSApplication.activate(ignoringOtherApps:)` is a no-op since macOS 14,
    /// so an overlay that spawns itself opens above everything but still sends
    /// keystrokes to whatever was frontmost. Activating the process from here,
    /// once it has finished launching, is the supported path.
    private func activate(pid: pid_t, attempts: Int) {
        guard attempts > 0 else {
            FileHandle.standardError.write(Data("rewindd: rewind-ui did not activate\n".utf8))
            return
        }
        guard let application = NSRunningApplication(processIdentifier: pid),
            application.isFinishedLaunching
        else {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { [weak self] in
                self?.activate(pid: pid, attempts: attempts - 1)
            }
            return
        }
        application.activate(options: [.activateAllWindows])
    }
}
