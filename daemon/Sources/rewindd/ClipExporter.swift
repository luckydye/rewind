import AVFoundation
import CoreImage
import CoreMedia
import Foundation

enum ClipExportError: Error, CustomStringConvertible {
    case emptyClip
    case missingFormatDescription
    case writerRejectedInput
    case writerFailed(String)
    case annotationExportFailed(String)

    var description: String {
        switch self {
        case .emptyClip: return "nothing to export"
        case .missingFormatDescription: return "clip has no format description"
        case .writerRejectedInput: return "AVAssetWriter rejected the video input"
        case .writerFailed(let message): return "AVAssetWriter failed: \(message)"
        case .annotationExportFailed(let message):
            return "could not burn the annotations in: \(message)"
        }
    }
}

struct ExportResult {
    let url: URL
    let duration: TimeInterval
    let bytes: Int
}

/// Muxes already-encoded samples into an MP4 without touching the encoder.
/// Export is user-initiated, so a blocking write on a background queue is fine.
enum ClipExporter {
    static func write(frames: [CMSampleBuffer], to url: URL) throws -> ExportResult {
        // AVFoundation objects are autoreleased and this runs off the main
        // thread, so give them a pool to drain into.
        try autoreleasepool {
        guard let first = frames.first, let last = frames.last else {
            throw ClipExportError.emptyClip
        }
        guard let format = CMSampleBufferGetFormatDescription(first) else {
            throw ClipExportError.missingFormatDescription
        }

        try? FileManager.default.removeItem(at: url)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )

        let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: nil, sourceFormatHint: format)
        input.expectsMediaDataInRealTime = false
        guard writer.canAdd(input) else { throw ClipExportError.writerRejectedInput }
        writer.add(input)

        guard writer.startWriting() else {
            throw ClipExportError.writerFailed(writer.error?.localizedDescription ?? "startWriting")
        }
        writer.startSession(atSourceTime: first.presentationTimeStamp)

        let feeder = Feeder(input: input, frames: frames)
        let done = DispatchSemaphore(value: 0)
        input.requestMediaDataWhenReady(on: DispatchQueue(label: "com.rewind.export")) {
            feeder.pump(signal: done)
        }
        done.wait()

        if let error = feeder.error {
            writer.cancelWriting()
            throw ClipExportError.writerFailed(error)
        }

        input.markAsFinished()
        let finished = DispatchSemaphore(value: 0)
        writer.finishWriting { finished.signal() }
        finished.wait()

        guard writer.status == .completed else {
            throw ClipExportError.writerFailed(writer.error?.localizedDescription ?? "unknown")
        }

        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        return ExportResult(
            url: url,
            duration: last.presentationTimeStamp.seconds - first.presentationTimeStamp.seconds,
            bytes: (attributes[.size] as? Int) ?? 0
        )
        }
    }

    /// Burns a static annotation layer into the clip. Export with annotations
    /// has to re-encode, so the untouched clip is muxed first and then passed
    /// through a Core Image video composition.
    static func writeAnnotated(
        frames: [CMSampleBuffer],
        overlay: AnnotationOverlay,
        to url: URL
    ) async throws -> ExportResult {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent("rewind-unannotated-\(UUID().uuidString).mp4")
        defer { try? FileManager.default.removeItem(at: temporary) }

        let raw = try write(frames: frames, to: temporary)
        let asset = AVURLAsset(url: raw.url)
        let annotation = CIImage(cgImage: try overlay.cgImage())

        let composition = AVMutableVideoComposition(asset: asset) { request in
            let source = request.sourceImage.clampedToExtent()
            let scaleX = source.extent.width / annotation.extent.width
            let scaleY = source.extent.height / annotation.extent.height
            let scaled = annotation
                .transformed(by: CGAffineTransform(scaleX: scaleX, y: scaleY))
                .composited(over: source)
                .cropped(to: request.sourceImage.extent)
            request.finish(with: scaled, context: nil)
        }

        guard let session = AVAssetExportSession(
            asset: asset,
            presetName: AVAssetExportPresetHEVCHighestQuality
        ) else {
            throw ClipExportError.annotationExportFailed("no HEVC export session")
        }
        session.videoComposition = composition

        try? FileManager.default.removeItem(at: url)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        do {
            try await session.export(to: url, as: .mp4)
        } catch {
            throw ClipExportError.annotationExportFailed(error.localizedDescription)
        }

        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        return ExportResult(
            url: url,
            duration: raw.duration,
            bytes: (attributes[.size] as? Int) ?? 0
        )
    }

    private final class Feeder {
        private let input: AVAssetWriterInput
        private let frames: [CMSampleBuffer]
        private var index = 0
        private var isDraining = false
        private(set) var error: String?

        init(input: AVAssetWriterInput, frames: [CMSampleBuffer]) {
            self.input = input
            self.frames = frames
        }

        func pump(signal: DispatchSemaphore) {
            guard !isDraining else { return }
            while input.isReadyForMoreMediaData {
                if index == frames.count {
                    isDraining = true
                    input.markAsFinished()
                    signal.signal()
                    return
                }
                if !input.append(frames[index]) {
                    error = "append failed at frame \(index)"
                    isDraining = true
                    signal.signal()
                    return
                }
                index += 1
            }
        }
    }
}
