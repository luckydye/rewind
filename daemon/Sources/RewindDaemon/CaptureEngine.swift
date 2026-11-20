import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
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
/// queue, so the encoder and the ring need no internal synchronisation beyond
/// what they already do.
final class CaptureEngine: NSObject, SCStreamOutput, SCStreamDelegate {
    private let options: RunOptions
    private let encoder: VideoEncoder
    private let queue = DispatchQueue(label: "com.rewind.capture", qos: .userInitiated)
    private var stream: SCStream?

    private(set) var receivedFrames = 0
    private(set) var idleNotices = 0
    private(set) var blankNotices = 0
    private(set) var droppedFrames = 0
    private(set) var outputSize: (width: Int, height: Int) = (0, 0)
    private(set) var displayName = ""

    init(options: RunOptions, encoder: VideoEncoder) {
        self.options = options
        self.encoder = encoder
    }

    func start() async throws {
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: true
        )

        guard !content.displays.isEmpty else { throw CaptureError.noDisplay }
        guard options.displayIndex < content.displays.count else {
            throw CaptureError.displayIndexOutOfRange(
                requested: options.displayIndex,
                available: content.displays.count
            )
        }

        let display = content.displays[options.displayIndex]
        displayName = "#\(options.displayIndex) (id \(display.displayID))"
        let filter = try makeFilter(content: content, display: display)
        let configuration = makeConfiguration(for: display)

        let size = outputDimensions(for: display)
        outputSize = size
        try encoder.prepare(
            width: size.width,
            height: size.height,
            fps: options.fps,
            bitrate: options.bitrate
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
    }

    // MARK: - SCStreamOutput

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen else { return }
        guard sampleBuffer.isValid else { return }

        switch sampleBuffer.frameStatus {
        case .complete:
            receivedFrames += 1
            guard let pixelBuffer = sampleBuffer.imageBuffer else {
                droppedFrames += 1
                return
            }
            encoder.encode(pixelBuffer, presentationTime: sampleBuffer.presentationTimeStamp)
        case .idle:
            // Nothing changed on screen: no frame was produced, nothing to encode.
            idleNotices += 1
        case .blank:
            blankNotices += 1
        default:
            break
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        FileHandle.standardError.write(
            Data("rewindd: capture stream stopped: \(error)\n".utf8)
        )
        exit(1)
    }

    // MARK: - Setup

    private func makeFilter(content: SCShareableContent, display: SCDisplay) throws -> SCContentFilter {
        guard let bundleID = options.applicationBundleID else {
            return SCContentFilter(display: display, excludingWindows: [])
        }
        guard let application = content.applications.first(where: { $0.bundleIdentifier == bundleID }) else {
            throw CaptureError.applicationNotRunning(bundleID)
        }
        return SCContentFilter(
            display: display,
            including: [application],
            exceptingWindows: []
        )
    }

    private func makeConfiguration(for display: SCDisplay) -> SCStreamConfiguration {
        let size = outputDimensions(for: display)
        let configuration = SCStreamConfiguration()
        configuration.width = size.width
        configuration.height = size.height
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(options.fps))
        // Queue depth 3: under load prefer dropping frames over stalling the compositor.
        configuration.queueDepth = 3
        configuration.showsCursor = options.showsCursor
        configuration.scalesToFit = true
        configuration.preservesAspectRatio = true
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        configuration.captureResolution = options.captureResolution.sckValue
        configuration.capturesAudio = false
        return configuration
    }

    private func outputDimensions(for display: SCDisplay) -> (width: Int, height: Int) {
        let pixelWidth = CGDisplayPixelsWide(display.displayID)
        let pixelHeight = CGDisplayPixelsHigh(display.displayID)
        guard pixelWidth > 0, pixelHeight > 0, options.height > 0 else {
            return (1920, 1080)
        }
        let scale = min(1.0, Double(options.height) / Double(pixelHeight))
        return (
            even(Int((Double(pixelWidth) * scale).rounded())),
            even(Int((Double(pixelHeight) * scale).rounded()))
        )
    }
}

private func even(_ value: Int) -> Int {
    value % 2 == 0 ? value : value - 1
}

private extension SCCaptureResolution {
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
