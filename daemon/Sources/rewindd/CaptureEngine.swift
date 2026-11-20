import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import RewindKit
import ScreenCaptureKit

enum CaptureError: Error, CustomStringConvertible {
    case noDisplay
    case displayIndexOutOfRange(requested: Int, available: Int)
    case applicationNotRunning(String)
    case streamStartFailed(String)

    var description: String {
        switch self {
        case .noDisplay:
            return "no display is available for capture"
        case .displayIndexOutOfRange(let requested, let available):
            return "display \(requested) does not exist (\(available) available)"
        case .applicationNotRunning(let bundleID):
            return "application \(bundleID) is not running"
        case .streamStartFailed(let message):
            return "SCStream failed to start: \(message)"
        }
    }
}

/// Owns the SCStream. Everything below `sampleHandlerQueue` runs on one serial
/// queue, so the encoder and the ring need no queue affinity of their own.
final class CaptureEngine: NSObject, SCStreamOutput, SCStreamDelegate {
    private let config: RecorderConfig
    private let encoder: VideoEncoder
    private let stats: CaptureStats
    private let onFailure: (Error) -> Void
    private let queue = DispatchQueue(label: "com.rewind.capture", qos: .userInitiated)
    private var stream: SCStream?

    init(
        config: RecorderConfig,
        encoder: VideoEncoder,
        stats: CaptureStats,
        onFailure: @escaping (Error) -> Void
    ) {
        self.config = config
        self.encoder = encoder
        self.stats = stats
        self.onFailure = onFailure
    }

    func start() async throws {
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: true
        )

        guard !content.displays.isEmpty else { throw CaptureError.noDisplay }
        guard config.displayIndex < content.displays.count else {
            throw CaptureError.displayIndexOutOfRange(
                requested: config.displayIndex,
                available: content.displays.count
            )
        }

        let display = content.displays[config.displayIndex]
        let filter = try makeFilter(content: content, display: display)
        let configuration = makeConfiguration(for: display)

        let size = CaptureEngine.outputDimensions(for: display, maxHeight: config.height)
        try encoder.prepare(
            width: size.width,
            height: size.height,
            fps: config.fps,
            bitrate: config.bitrate
        )

        let stream = SCStream(filter: filter, configuration: configuration, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        try await stream.startCapture()
        self.stream = stream
    }

    func stop() async throws {
        guard let stream else { return }
        self.stream = nil
        try await stream.stopCapture()
        encoder.finish()
    }

    // MARK: - SCStreamOutput

    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of type: SCStreamOutputType
    ) {
        guard type == .screen, sampleBuffer.isValid else { return }

        switch sampleBuffer.frameStatus {
        case .complete:
            stats.recordCaptured()
            guard let pixelBuffer = sampleBuffer.imageBuffer else {
                stats.recordDropped()
                return
            }
            encoder.encode(pixelBuffer, presentationTime: sampleBuffer.presentationTimeStamp)
        case .idle:
            // Nothing changed on screen: no frame was produced, nothing to encode.
            stats.recordIdle()
        case .blank:
            stats.recordBlank()
        default:
            break
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        onFailure(error)
    }

    // MARK: - Setup

    private func makeFilter(content: SCShareableContent, display: SCDisplay) throws -> SCContentFilter {
        guard let bundleID = config.applicationBundleID else {
            return SCContentFilter(display: display, excludingWindows: [])
        }
        guard let application = content.applications.first(where: { $0.bundleIdentifier == bundleID })
        else {
            throw CaptureError.applicationNotRunning(bundleID)
        }
        return SCContentFilter(display: display, including: [application], exceptingWindows: [])
    }

    private func makeConfiguration(for display: SCDisplay) -> SCStreamConfiguration {
        let size = CaptureEngine.outputDimensions(for: display, maxHeight: config.height)
        let configuration = SCStreamConfiguration()
        configuration.width = size.width
        configuration.height = size.height
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(config.fps))
        // Queue depth 3: under load prefer dropping frames over stalling the compositor.
        configuration.queueDepth = 3
        configuration.showsCursor = config.showsCursor
        configuration.scalesToFit = true
        configuration.preservesAspectRatio = true
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        configuration.captureResolution = config.captureResolution.sckValue
        configuration.capturesAudio = false
        return configuration
    }

    static func outputDimensions(for display: SCDisplay, maxHeight: Int) -> (width: Int, height: Int) {
        let pixelWidth = CGDisplayPixelsWide(display.displayID)
        let pixelHeight = CGDisplayPixelsHigh(display.displayID)
        guard pixelWidth > 0, pixelHeight > 0, maxHeight > 0 else {
            return (1920, 1080)
        }
        let scale = min(1.0, Double(maxHeight) / Double(pixelHeight))
        return (
            even(Int((Double(pixelWidth) * scale).rounded())),
            even(Int((Double(pixelHeight) * scale).rounded()))
        )
    }

    private static func even(_ value: Int) -> Int {
        value % 2 == 0 ? value : value - 1
    }
}

private extension CaptureResolution {
    var sckValue: SCCaptureResolutionType {
        switch self {
        case .automatic: return .automatic
        case .best: return .best
        case .nominal: return .nominal
        }
    }
}

private extension CMSampleBuffer {
    var frameStatus: SCFrameStatus {
        guard
            let attachments = CMSampleBufferGetSampleAttachmentsArray(self, createIfNecessary: false)
                as? [[String: Any]],
            let raw = attachments.first?[SCStreamFrameInfo.status.rawValue] as? Int,
            let status = SCFrameStatus(rawValue: raw)
        else {
            return .complete
        }
        return status
    }
}
