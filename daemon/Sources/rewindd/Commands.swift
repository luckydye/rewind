import CoreGraphics
import Foundation
import RewindKit
import ScreenCaptureKit

final class StopFlag {
    private let lock = NSLock()
    private var stopped = false

    var isSet: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stopped
    }

    func set() {
        lock.lock()
        stopped = true
        lock.unlock()
    }
}

enum Commands {
    static func list() async throws {
        for display in try await ShareableContent.displays() {
            let main = display.isMain ? " (main)" : ""
            print("  [\(display.index)] \(display.width)x\(display.height)\(main)")
        }
        print("")
        for application in try await ShareableContent.applications() {
            print("  \(application.name) — \(application.bundleIdentifier)")
        }
    }

    static func doctor() async throws {
        print("macOS: \(ProcessInfo.processInfo.operatingSystemVersionString)")
        print("screen recording permission: \(CGPreflightScreenCaptureAccess() ? "granted" : "NOT granted")")
        print("displays: \((try await ShareableContent.displays()).count)")

        let encoder = VideoEncoder(stats: CaptureStats()) { _ in }
        try encoder.prepare(width: 1920, height: 1080, fps: 30, bitrate: 5_000_000)
        print("HEVC hardware encoder: \(encoder.usesHardwareAcceleration ? "yes" : "NO (software fallback)")")
    }

    static func run(_ options: RunOptions) async throws {
        let recorder = Recorder(config: options.config)

        let stop = StopFlag()
        let signalSource = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
        signal(SIGINT, SIG_IGN)
        signalSource.setEventHandler { stop.set() }
        signalSource.resume()

        let state = await recorder.start()
        print(
            """
            rewinding: \(state.display?.name ?? "display ?") \
            \(state.width)x\(state.height)@\(state.config.fps) \
            buffer=\(Int(state.bufferSeconds))s
            """
        )

        let started = Date()
        var lastReport = started
        while !stop.isSet {
            if let limit = options.runSeconds, Date().timeIntervalSince(started) >= limit {
                break
            }
            try await Task.sleep(for: .milliseconds(100))

            if options.statisticsInterval > 0,
                Date().timeIntervalSince(lastReport) >= options.statisticsInterval
            {
                lastReport = Date()
                print(statisticsLine(await recorder.state()))
            }
        }

        let stopped = await recorder.stop()
        signalSource.cancel()
        print(statisticsLine(stopped))

        if let path = options.exportPath {
            let clip = try await recorder.export(seconds: options.clipSeconds, path: path)
            print(
                String(
                    format: "exported %@ — %.2fs, %.1fMB, %d frames of %dx%d",
                    clip.path,
                    clip.duration,
                    Double(clip.bytes) / 1_048_576,
                    clip.frames,
                    clip.width,
                    clip.height
                )
            )
        }
    }

    static func statisticsLine(_ state: State) -> String {
        String(
            format: "in=%-5d out=%-5d drop=%-4d idle=%-5d ring=%4.1fs %5.1fMB avg=%4.1fMbps",
            state.framesCaptured,
            state.framesEncoded,
            state.droppedFrames,
            state.idleNotices,
            state.availableSeconds,
            Double(state.bufferBytes) / 1_048_576,
            state.bitrate / 1_000_000
        )
    }
}
