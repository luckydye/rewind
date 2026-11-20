import AppKit
import Foundation

/// Launches `rewind-ui`, which lives next to the daemon executable — inside
/// `Rewind.app/Contents/MacOS` in a bundle, and beside `rewindd` during
/// development.
final class UILauncher {
    private var process: Process?

    var isRunning: Bool {
        process?.isRunning == true
    }

    func open() {
        if isRunning { return }

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
        process.terminationHandler = { [weak self] _ in
            self?.process = nil
        }
        do {
            try process.run()
            self.process = process
            activate(pid: process.processIdentifier, attempts: 40)
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
        guard let application = NSRunningApplication(processIdentifier: pid) else {
            retry(pid: pid, attempts: attempts)
            return
        }
        guard application.isFinishedLaunching else {
            retry(pid: pid, attempts: attempts)
            return
        }
        application.activate(options: [.activateAllWindows])
    }

    private func retry(pid: pid_t, attempts: Int) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { [weak self] in
            self?.activate(pid: pid, attempts: attempts - 1)
        }
    }
}
