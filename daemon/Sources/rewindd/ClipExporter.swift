import AVFoundation
import CoreImage
import CoreMedia
import Foundation
import RewindKit

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

    /// Burns annotations in and/or crops the clip. Either needs a re-encode, so
    /// the untouched clip is muxed first and then passed through a Core Image
    /// video composition.
    static func writeComposed(
        frames: [CMSampleBuffer],
        overlay: AnnotationOverlay?,
        crop: Crop?,
        to url: URL
    ) async throws -> ExportResult {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent("rewind-unannotated-\(UUID().uuidString).mp4")
        defer { try? FileManager.default.removeItem(at: temporary) }

        let raw = try write(frames: frames, to: temporary)
        let asset = AVURLAsset(url: raw.url)
        let annotation = try overlay.map { CIImage(cgImage: try $0.cgImage()) }

        let composition = AVMutableVideoComposition(asset: asset) { request in
            let source = request.sourceImage
            var image = source
            if let annotation {
                let scaleX = source.extent.width / annotation.extent.width
                let scaleY = source.extent.height / annotation.extent.height
                image = annotation
                    .transformed(by: CGAffineTransform(scaleX: scaleX, y: scaleY))
                    .composited(over: source)
                    .cropped(to: source.extent)
            }
            if let crop {
                // Core Image's origin is bottom-left; the crop is measured from
                // the top, like the pointer that drew it.
                let rect = crop.pixels(width: Int(source.extent.width), height: Int(source.extent.height))
                let inImage = CGRect(
                    x: rect.origin.x,
                    y: source.extent.height - rect.maxY,
                    width: rect.width,
                    height: rect.height
                )
                image = image
                    .cropped(to: inImage)
                    .transformed(by: CGAffineTransform(translationX: -inImage.minX, y: -inImage.minY))
            }
            request.finish(with: image, context: nil)
        }
        if let crop {
            let rect = crop.pixels(
                width: Int(composition.renderSize.width),
                height: Int(composition.renderSize.height)
            )
            composition.renderSize = CGSize(width: rect.width, height: rect.height)
        }

        guard let session = AVAssetExportSession(
            asset: asset,
            presetName: AVAssetExportPresetHighestQuality
        ) else {
            throw ClipExportError.annotationExportFailed("no export session")
        }
        session.videoComposition = composition
        session.outputURL = url
        session.outputFileType = .mp4

        try? FileManager.default.removeItem(at: url)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )

        let failure: String? = await withCheckedContinuation { continuation in
            // The async `export(to:as:)` path silently drops `videoComposition`
            // on macOS 15/26; the callback-based path applies it correctly.
            session.exportAsynchronously {
                switch session.status {
                case .completed:
                    continuation.resume(returning: nil)
                case .failed, .cancelled:
                    continuation.resume(
                        returning: session.error?.localizedDescription ?? "export was cancelled"
                    )
                default:
                    continuation.resume(returning: "export ended in status \(session.status.rawValue)")
                }
            }
        }
        if let failure {
            throw ClipExportError.annotationExportFailed(failure)
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
