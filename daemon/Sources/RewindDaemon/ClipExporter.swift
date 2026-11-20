import AVFoundation
import CoreMedia
import Foundation

enum ClipExportError: Error, CustomStringConvertible {
    case emptyClip
    case missingFormatDescription
    case writerRejectedInput
    case writerFailed(String)

    var description: String {
        switch self {
        case .emptyClip: return "nothing to export"
        case .missingFormatDescription: return "clip has no format description"
        case .writerRejectedInput: return "AVAssetWriter rejected the video input"
        case .writerFailed(let message): return "AVAssetWriter failed: \(message)"
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
