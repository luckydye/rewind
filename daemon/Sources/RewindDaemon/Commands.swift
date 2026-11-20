import CoreGraphics
import Foundation
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
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        print("displays:")
        for (index, display) in content.displays.enumerated() {
            let pixels = "\(CGDisplayPixelsWide(display.displayID))x\(CGDisplayPixelsHigh(display.displayID))"
            print("  [\(index)] id=\(display.displayID) pixels=\(pixels) frame=\(display.frame)")
        }
        print("\ncapturable applications:")
        for application in content.applications.sorted(by: { $0.applicationName < $1.applicationName }) {
            print("  \(application.applicationName) — \(application.bundleIdentifier)")
        }
    }

    static func doctor() async throws {
        print("macOS: \(ProcessInfo.processInfo.operatingSystemVersionString)")
        print("screen recording permission: \(CGPreflightScreenCaptureAccess() ? "granted" : "NOT granted")")

        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        print("displays: \(content.displays.count)")

        let encoder = VideoEncoder { _ in }
        try encoder.prepare(width: 1920, height: 1080, fps: 30, bitrate: 5_000_000)
        print("HEVC hardware encoder: \(encoder.usesHardwareAcceleration ? "yes" : "NO (software fallback)")")
    }

    static func run(_ options: RunOptions) async throws {
        let ring = FrameRing(capacity: options.bufferSeconds)
        let encoder = VideoEncoder { ring.append($0) }
        let engine = CaptureEngine(options: options, encoder: encoder)

        let stop = StopFlag()
        let signalSource = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
        signal(SIGINT, SIG_IGN)
        signalSource.setEventHandler { stop.set() }
        signalSource.resume()

        try await engine.start()

        let started = Date()
        print(
            """
            rewinding: display=\(engine.displayName) \
            \(engine.outputSize.width)x\(engine.outputSize.height)@\(options.fps) \
            buffer=\(Int(options.bufferSeconds))s \
            hardware=\(encoder.usesHardwareAcceleration ? "yes" : "no")
            """
        )

        var lastReport = started
        while !stop.isSet {
            if let limit = options.runSeconds, Date().timeIntervalSince(started) >= limit {
                break
            }
            try await Task.sleep(for: .milliseconds(100))

            let interval = options.statisticsInterval
            if interval > 0, Date().timeIntervalSince(lastReport) >= interval {
                lastReport = Date()
                print(statisticsLine(engine: engine, encoder: encoder, ring: ring))
            }
        }

        try await engine.stop()
        signalSource.cancel()
        print(statisticsLine(engine: engine, encoder: encoder, ring: ring))

        if let path = options.exportPath {
            try await exportClip(ring: ring, seconds: options.clipSeconds, path: path, encoder: encoder)
        }
    }

    static func statisticsLine(engine: CaptureEngine, encoder: VideoEncoder, ring: FrameRing) -> String {
        let (bytes, duration, _) = ring.statistics()
        let megawatts = duration > 0 ? Double(bytes) * 8 / duration / 1_000_000 : 0
        return String(
            format: "in=%-5d out=%-5d drop=%-4d idle=%-5d ring=%4.1fs %5.1fMB avg=%4.1fMbps",
            engine.receivedFrames,
            encoder.encodedFrames,
            engine.droppedFrames,
            engine.idleNotices,
            duration,
            Double(bytes) / 1_048_576,
            megawatts
        )
    }

    static func exportClip(ring: FrameRing, seconds: Double, path: String, encoder: VideoEncoder) async throws {
        let snapshot = try ring.snapshot(last: seconds)
        let url = URL(fileURLWithPath: path)
        let result = try await Task.detached {
            try ClipExporter.write(frames: snapshot.frames, to: url)
        }.value
        print(
            String(
                format: "exported %@ — %.2fs, %.1fMB, %d frames of %dx%d",
                result.url.path,
                result.duration,
                Double(result.bytes) / 1_048_576,
                snapshot.frames.count,
                encoder.width,
                encoder.height
            )
        )
    }
}
