import Foundation
import RewindKit

@main
struct Rewindd {
    static func main() {
        do {
            switch try Options.parse(CommandLine.arguments) {
            case .daemon:
                try Daemon.run()
            case .loginItem(let enabled):
                try Commands.loginItem(enabled: enabled)
            case .list:
                try runBlocking { try await Commands.list() }
            case .doctor:
                try runBlocking { try await Commands.doctor() }
            case .run(let options):
                try runBlocking { try await Commands.run(options) }
            }
        } catch let error as OptionsError {
            FileHandle.standardError.write(Data("rewindd: \(error)\n\n\(Options.usage)\n".utf8))
            exit(2)
        } catch {
            FileHandle.standardError.write(Data("rewindd: \(error)\n".utf8))
            exit(1)
        }
    }
}

/// The daemon owns the main thread with `NSApplication.run()`, so the debugging
/// subcommands drive their async work from a detached task and block on it.
private func runBlocking(_ operation: @escaping () async throws -> Void) throws {
    let finished = DispatchSemaphore(value: 0)
    var failure: Error?

    Task.detached {
        do {
            try await operation()
        } catch {
            failure = error
        }
        finished.signal()
    }

    finished.wait()
    if let failure {
        throw failure
    }
}
