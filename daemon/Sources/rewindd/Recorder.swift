import CoreMedia
import Foundation
import RewindKit

enum RecorderError: Error, CustomStringConvertible {
    case notRecording

    var description: String {
        switch self {
        case .notRecording: return "rewindd is not recording"
        }
    }
}

/// Owns the capture stack and serialises every lifecycle transition. All
/// mutations are actor-isolated, so a menu click and an IPC request can never
/// start and stop the stream concurrently.
actor Recorder {
    private var config: RecorderConfig
    private var ring: FrameRing?
    private var encoder: VideoEncoder?
    private var engine: CaptureEngine?
    private var stats = CaptureStats()
    private var display: DisplayInfo?
    private var desiredRecording = false
    private var lastError: String?
    private var retryTask: Task<Void, Never>?
    private let previews = PreviewEngine()

    init(config: RecorderConfig) {
        self.config = config
    }

    func start() async -> State {
        desiredRecording = true
        cancelRetry()
        if engine == nil {
            await beginCapture()
        }
        return state()
    }

    func stop() async -> State {
        desiredRecording = false
        cancelRetry()
        await endCapture()
        lastError = nil
        return state()
    }

    func apply(_ newConfig: RecorderConfig) async -> State {
        guard newConfig != config else { return state() }
        let shouldResume = desiredRecording || engine != nil
        cancelRetry()
        await endCapture()
        config = newConfig
        ConfigStore.save(config)
        if shouldResume {
            desiredRecording = true
            await beginCapture()
        }
        return state()
    }

    func state() -> State {
        let counters = stats.snapshot()
        let ringStats = ring?.statistics() ?? (bytes: 0, duration: 0, frames: 0)
        let size = encoder.map { ($0.width, $0.height) } ?? (0, 0)

        return State(
            recording: engine != nil,
            config: config,
            display: display,
            width: size.0,
            height: size.1,
            bufferSeconds: config.bufferSeconds,
            availableSeconds: ringStats.duration,
            bufferBytes: ringStats.bytes,
            bitrate: ringStats.duration > 0
                ? Double(ringStats.bytes) * 8 / ringStats.duration
                : 0,
            framesCaptured: counters.captured,
            framesEncoded: counters.encoded,
            droppedFrames: counters.dropped,
            idleNotices: counters.idle,
            onBattery: PowerSource.isOnBattery,
            lastError: lastError
        )
    }

    func export(from: Double, to: Double, path: String?) async throws -> Clip {
        guard let ring, let encoder else { throw RecorderError.notRecording }
        let snapshot = try ring.snapshot(from: from, to: to)
        let url = path.map { URL(fileURLWithPath: $0) } ?? Self.newClipURL()

        let result = try await Task.detached {
            try ClipExporter.write(frames: snapshot.frames, to: url)
        }.value

        return Clip(
            path: result.url.path,
            duration: result.duration,
            bytes: result.bytes,
            frames: snapshot.frames.count,
            width: encoder.width,
            height: encoder.height
        )
    }

    func preview(secondsAgo: Double, width: Int) async throws -> RawImage {
        guard let ring else { throw RecorderError.notRecording }
        let snapshot = try ring.snapshot(last: config.bufferSeconds)
        let index = Self.frameIndex(secondsAgo: secondsAgo, frames: snapshot.frames)
        return try previews.frame(frames: snapshot.frames, index: index, width: width)
    }

    static func frameIndex(secondsAgo: Double, frames: [CMSampleBuffer]) -> Int {
        guard let last = frames.last else { return 0 }
        let target = last.presentationTimeStamp.seconds - secondsAgo
        for index in stride(from: frames.count - 1, through: 0, by: -1)
        where frames[index].presentationTimeStamp.seconds <= target {
            return index
        }
        return 0
    }

    func displays() async throws -> [DisplayInfo] {
        try await ShareableContent.displays()
    }

    func applications() async throws -> [ApplicationInfo] {
        try await ShareableContent.applications()
    }

    // MARK: - Lifecycle

    private func beginCapture() async {
        do {
            let ring = FrameRing(capacity: config.bufferSeconds)
            let stats = CaptureStats()
            let encoder = VideoEncoder(stats: stats) { ring.append($0) }
            let engine = CaptureEngine(
                config: config,
                encoder: encoder,
                stats: stats
            ) { [weak self] error in
                Task { await self?.captureFailed(error) }
            }

            try await engine.start()

            self.ring = ring
            self.encoder = encoder
            self.engine = engine
            self.stats = stats
            self.display = try? await ShareableContent.displays()
                .first { $0.index == config.displayIndex }
            self.lastError = nil
        } catch {
            lastError = "\(error)"
            await endCapture()
            scheduleRetry()
        }
    }

    private func endCapture() async {
        if let engine {
            try? await engine.stop()
        }
        engine = nil
        encoder?.finish()
        encoder = nil
        ring = nil
    }

    private func captureFailed(_ error: Error) async {
        lastError = "\(error)"
        await endCapture()
        if desiredRecording {
            scheduleRetry()
        }
    }

    private func cancelRetry() {
        retryTask?.cancel()
        retryTask = nil
    }

    private func scheduleRetry() {
        cancelRetry()
        retryTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(5))
            guard !Task.isCancelled else { return }
            await self?.retry()
        }
    }

    private func retry() async {
        retryTask = nil
        guard desiredRecording, engine == nil else { return }
        await beginCapture()
    }

    private static func newClipURL() -> URL {
        let movies = FileManager.default.urls(for: .moviesDirectory, in: .userDomainMask)[0]
        let directory = movies.appendingPathComponent("Rewind", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd HH.mm.ss"
        return directory.appendingPathComponent("Rewind \(formatter.string(from: Date())).mp4")
    }
}
