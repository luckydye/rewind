import AVFoundation
import CoreImage
import CoreMedia
import Foundation
import ImageIO
import RewindKit
import UniformTypeIdentifiers

enum GifExportError: Error, CustomStringConvertible {
    case invalidOptions
    case noVideoTrack
    case readerFailed(String)
    case renderFailed
    case destinationFailed
    case frameCountMismatch(planned: Int, written: Int)

    var description: String {
        switch self {
        case .invalidOptions: return "GIF fps and width must be positive"
        case .noVideoTrack: return "the clip has no video track"
        case .readerFailed(let message): return "could not decode the clip: \(message)"
        case .renderFailed: return "could not render a GIF frame"
        case .destinationFailed: return "could not write the GIF"
        case .frameCountMismatch(let planned, let written):
            return "GIF has \(written) of \(planned) planned frames"
        }
    }
}

/// One GIF frame: which captured frame it shows, and for how long.
struct GifFrame {
    let index: Int
    /// Seconds, already a whole number of centiseconds, which is all GIF stores.
    let delay: Double
}

enum GifExporter {
    /// Capture is variable-rate: an unchanged screen produces no frames at all.
    /// So rather than resampling on a fixed clock, the first frame in each
    /// 1/fps slot is kept, shown from the start of its slot, and stands until
    /// the next kept frame. Snapping to slots keeps delays even; exact capture
    /// times made 2 cs frames after every idle gap. Delays come from rounded
    /// timestamps, not rounded durations, so rounding does not add up.
    static func timeline(times: [Double], fps: Double) -> [GifFrame] {
        guard let first = times.first, let last = times.last else { return [] }
        var kept: [(index: Int, time: Double)] = []
        var lastSlot = -1.0
        for (index, time) in times.enumerated() {
            let slot = ((time - first) * fps + 1e-6).rounded(.down)
            guard slot > lastSlot else { continue }
            kept.append((index, slot / fps))
            lastSlot = slot
        }
        let end = last - first + 1 / fps
        let centiseconds = { (t: Double) in (t * 100).rounded() }
        return kept.enumerated().map { position, frame in
            let until = position + 1 < kept.count ? kept[position + 1].time : end
            // Browsers play delays under 2 cs at 10 cs; never write one.
            let delay = max(2, centiseconds(until) - centiseconds(frame.time))
            return GifFrame(index: frame.index, delay: delay / 100)
        }
    }

    /// Muxes the clip, decodes it in presentation order, burns in the drawing,
    /// crops, scales to `maxWidth`, and lets ImageIO quantise each frame.
    static func write(
        frames: [CMSampleBuffer],
        overlay: AnnotationOverlay?,
        crop: Crop?,
        fps: Double,
        maxWidth: Int,
        to url: URL
    ) throws -> ExportResult {
        guard fps > 0, maxWidth > 0 else { throw GifExportError.invalidOptions }
        return try autoreleasepool {
        let temporary = FileManager.default.temporaryDirectory
            .appendingPathComponent("rewind-gif-source-\(UUID().uuidString).mp4")
        defer { try? FileManager.default.removeItem(at: temporary) }
        let raw = try ClipExporter.write(frames: frames, to: temporary)

        let asset = AVURLAsset(url: raw.url)
        guard let track = asset.tracks(withMediaType: .video).first else {
            throw GifExportError.noVideoTrack
        }
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(
            track: track,
            outputSettings: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA]
        )
        output.alwaysCopiesSampleData = false
        reader.add(output)
        guard reader.startReading() else {
            throw GifExportError.readerFailed(reader.error?.localizedDescription ?? "startReading")
        }

        // The reader yields frames in presentation order, so the plan is made
        // over sorted timestamps and matched by position.
        let plan = timeline(
            times: frames.map { $0.presentationTimeStamp.seconds }.sorted(),
            fps: fps
        )
        let delays = Dictionary(uniqueKeysWithValues: plan.map { ($0.index, $0.delay) })

        try? FileManager.default.removeItem(at: url)
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        guard let destination = CGImageDestinationCreateWithURL(
            url as CFURL,
            UTType.gif.identifier as CFString,
            plan.count,
            nil
        ) else {
            throw GifExportError.destinationFailed
        }
        CGImageDestinationSetProperties(destination, [
            kCGImagePropertyGIFDictionary: [kCGImagePropertyGIFLoopCount: 0],
        ] as CFDictionary)

        let annotation = try overlay.map { CIImage(cgImage: try $0.cgImage()) }
        let context = CIContext(options: [.cacheIntermediates: false])
        var index = 0
        var written = 0
        var size = (0, 0)
        while let sample = output.copyNextSampleBuffer() {
            guard let pixels = CMSampleBufferGetImageBuffer(sample) else { continue }
            defer { index += 1 }
            guard let delay = delays[index] else { continue }

            try autoreleasepool {
                let image = scaled(
                    ClipExporter.compose(CIImage(cvPixelBuffer: pixels), annotation: annotation, crop: crop),
                    maxWidth: maxWidth
                )
                guard let frame = context.createCGImage(image, from: image.extent) else {
                    throw GifExportError.renderFailed
                }
                CGImageDestinationAddImage(destination, frame, [
                    kCGImagePropertyGIFDictionary: [
                        kCGImagePropertyGIFDelayTime: delay,
                        kCGImagePropertyGIFUnclampedDelayTime: delay,
                    ],
                ] as CFDictionary)
                size = (frame.width, frame.height)
            }
            written += 1
        }
        guard reader.status == .completed else {
            throw GifExportError.readerFailed(reader.error?.localizedDescription ?? "status \(reader.status.rawValue)")
        }
        guard written == plan.count else {
            throw GifExportError.frameCountMismatch(planned: plan.count, written: written)
        }
        guard CGImageDestinationFinalize(destination) else {
            throw GifExportError.destinationFailed
        }

        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        return ExportResult(
            url: url,
            duration: raw.duration,
            bytes: (attributes[.size] as? Int) ?? 0,
            width: size.0,
            height: size.1
        )
        }
    }

    /// Lanczos, because screen content is mostly text and a box or bilinear
    /// filter smears it. The edge is clamped first so the filter does not pull
    /// transparent black in at the borders.
    static func scaled(_ image: CIImage, maxWidth: Int) -> CIImage {
        let width = image.extent.width
        guard width > CGFloat(maxWidth) else { return image }
        let scale = CGFloat(maxWidth) / width
        let target = CGRect(
            x: 0,
            y: 0,
            width: CGFloat(maxWidth),
            height: (image.extent.height * scale).rounded(.down)
        )
        return image
            .clampedToExtent()
            .applyingFilter("CILanczosScaleTransform", parameters: [
                kCIInputScaleKey: scale,
                kCIInputAspectRatioKey: 1,
            ])
            .cropped(to: target)
    }
}
