import CoreMedia
import CoreVideo
import Foundation
import RewindKit
import VideoToolbox

enum PreviewError: Error, CustomStringConvertible {
    case empty
    case noFormats
    case sessionFailed(OSStatus)
    case decodeFailed(OSStatus)
    case imageCreationFailed
    case scaleFailed(OSStatus)

    var description: String {
        switch self {
        case .empty: return "the rolling buffer is empty"
        case .noFormats: return "the rolling buffer holds no frame to decode from"
        case .sessionFailed(let status): return "VTDecompressionSessionCreate failed (\(status))"
        case .decodeFailed(let status): return "VTDecompressionSessionDecodeFrame failed (\(status))"
        case .imageCreationFailed: return "could not read the decoded pixel buffer"
        case .scaleFailed(let status): return "VTPixelTransferSession failed (\(status))"
        }
    }
}

/// Decodes encoded samples from the ring on demand at full resolution.
///
/// Decoder state is kept between calls: scrubbing forward costs one decode per
/// frame, and only a backward seek inside the same GOP costs a re-decode from
/// the keyframe. Without that, every scrub step would replay up to a second of
/// video.
final class PreviewEngine {
    private let lock = NSLock()
    private var session: VTDecompressionSession?
    private var format: CMVideoFormatDescription?
    private var decodedTime: CMTime?
    private var decodedBuffer: CVPixelBuffer?
    private var decodedStatus: OSStatus = noErr
    private var transfer: VTPixelTransferSession?

    deinit {
        if let session {
            VTDecompressionSessionInvalidate(session)
        }
        if let transfer {
            VTPixelTransferSessionInvalidate(transfer)
        }
    }

    /// `frames` must be ordered by presentation time and start at a keyframe.
    /// A `width` of zero means native resolution: never downscale a preview
    /// below the capture size, text stops being readable.
    func frame(frames: [CMSampleBuffer], index: Int, width: Int) throws -> RawImage {
        guard !frames.isEmpty else { throw PreviewError.empty }
        let bounded = min(max(index, 0), frames.count - 1)

        lock.lock()
        defer { lock.unlock() }

        try decode(frames: frames, through: bounded)
        guard let buffer = decodedBuffer else { throw PreviewError.imageCreationFailed }
        return try pixels(of: buffer, width: width)
    }

    /// Thumbnails come from keyframes, which decode without their neighbours.
    /// Resetting between them is what keeps a strip of 24 at 24 decodes rather
    /// than decoding every frame between the first and the last.
    func thumbnails(frames: [CMSampleBuffer], indices: [Int], width: Int) throws -> [RawImage] {
        var images: [RawImage] = []
        images.reserveCapacity(indices.count)
        for index in indices {
            resetDecoder()
            images.append(try frame(frames: frames, index: index, width: width))
        }
        return images
    }

    private func resetDecoder() {
        lock.lock()
        defer { lock.unlock() }
        if let session {
            VTDecompressionSessionInvalidate(session)
        }
        session = nil
        format = nil
        decodedTime = nil
        decodedBuffer = nil
    }

    private func decode(frames: [CMSampleBuffer], through target: Int) throws {
        guard let first = frames.first, let description = CMSampleBufferGetFormatDescription(first)
        else {
            throw PreviewError.noFormats
        }

        if format == nil || !CMFormatDescriptionEqual(format, otherFormatDescription: description) {
            reset(format: description)
        }

        var startIndex = keyframeIndex(frames: frames, atOrBefore: target)
        if let decodedTime, let existing = frames.firstIndex(where: {
            CMTimeCompare($0.presentationTimeStamp, decodedTime) == 0
        }) {
            // A static screen has no frames between two scrub positions, so the
            // same frame is asked for repeatedly. Resetting on `existing ==
            // target` would re-decode a whole GOP for every request.
            if existing >= startIndex && existing <= target {
                startIndex = existing + 1
            } else {
                reset(format: description)
            }
        } else if self.decodedTime != nil {
            reset(format: description)
        }

        guard startIndex <= target else { return }
        for index in startIndex...target {
            try decode(frame: frames[index])
        }

        if decodedBuffer == nil {
            throw PreviewError.imageCreationFailed
        }
    }

    private func decode(frame: CMSampleBuffer) throws {
        guard let session else { throw PreviewError.sessionFailed(-1) }
        decodedStatus = noErr
        var infoFlags = VTDecodeInfoFlags()

        let status = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: frame,
            flags: [],
            infoFlagsOut: &infoFlags,
            outputHandler: { [weak self] status, _, imageBuffer, _, _ in
                self?.receive(status: status, imageBuffer: imageBuffer)
            }
        )
        guard status == noErr, decodedStatus == noErr else {
            throw PreviewError.decodeFailed(status == noErr ? decodedStatus : status)
        }
        decodedTime = frame.presentationTimeStamp
    }

    private func receive(status: OSStatus, imageBuffer: CVImageBuffer?) {
        decodedStatus = status
        guard status == noErr, let imageBuffer else {
            decodedBuffer = nil
            return
        }
        decodedBuffer = imageBuffer
    }

    private func reset(format: CMVideoFormatDescription) {
        if let session {
            VTDecompressionSessionInvalidate(session)
        }
        session = nil
        decodedTime = nil
        decodedBuffer = nil
        self.format = format

        let attributes: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]

        var created: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: format,
            decoderSpecification: nil,
            imageBufferAttributes: attributes as CFDictionary,
            outputCallback: nil,
            decompressionSessionOut: &created
        )
        guard status == noErr else { return }
        session = created
    }

    /// Walks backwards and stops at the first keyframe: a full scan costs one
    /// CoreMedia attachment lookup per frame, which at 60 s of buffer and 30 fps
    /// dominates the whole request.
    private func keyframeIndex(frames: [CMSampleBuffer], atOrBefore target: Int) -> Int {
        for index in stride(from: target, through: 0, by: -1) where frames[index].isKeyframe {
            return index
        }
        return 0
    }

    // MARK: - Pixels

    private func pixels(of buffer: CVPixelBuffer, width: Int) throws -> RawImage {
        let nativeWidth = CVPixelBufferGetWidth(buffer)
        let nativeHeight = CVPixelBufferGetHeight(buffer)

        if width <= 0 || width >= nativeWidth {
            return try tightCopy(buffer, width: nativeWidth, height: nativeHeight)
        }
        return try scaledCopy(buffer, width: width, height: nativeHeight)
    }

    /// Fast path: one row copy per scanline, no CoreGraphics round trip.
    private func tightCopy(_ buffer: CVPixelBuffer, width: Int, height: Int) throws -> RawImage {
        CVPixelBufferLockBaseAddress(buffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }

        guard let base = CVPixelBufferGetBaseAddress(buffer) else {
            throw PreviewError.imageCreationFailed
        }
        let stride = CVPixelBufferGetBytesPerRow(buffer)
        let rowBytes = width * 4

        var pixels = Data(count: rowBytes * height)
        pixels.withUnsafeMutableBytes { destination in
            guard let out = destination.baseAddress else { return }
            for row in 0..<height {
                memcpy(out + row * rowBytes, base + row * stride, rowBytes)
            }
        }
        return RawImage(width: width, height: height, pixels: pixels)
    }

    /// The media engine's scaler rather than vImage: a Lanczos pass over a 5K
    /// frame took ~20 ms at preview size and ~45 ms per thumbnail on the CPU,
    /// which made the thumbnail strip hold the recorder for a second. Area
    /// averaging keeps text as legible as Lanczos did, in ~3 ms at any size.
    private func scaledCopy(_ buffer: CVPixelBuffer, width: Int, height: Int) throws -> RawImage {
        let sourceWidth = CVPixelBufferGetWidth(buffer)
        let sourceHeight = CVPixelBufferGetHeight(buffer)
        let scale = Double(width) / Double(sourceWidth)
        let scaledHeight = max(1, Int((Double(sourceHeight) * scale).rounded()))

        var scaled: CVPixelBuffer?
        let created = CVPixelBufferCreate(
            kCFAllocatorDefault,
            width,
            scaledHeight,
            kCVPixelFormatType_32BGRA,
            [kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary] as CFDictionary,
            &scaled
        )
        guard created == kCVReturnSuccess, let scaled else { throw PreviewError.imageCreationFailed }

        let status = VTPixelTransferSessionTransferImage(try scaler(), from: buffer, to: scaled)
        guard status == noErr else { throw PreviewError.scaleFailed(status) }
        return try tightCopy(scaled, width: width, height: scaledHeight)
    }

    private func scaler() throws -> VTPixelTransferSession {
        if let transfer { return transfer }
        var created: VTPixelTransferSession?
        let status = VTPixelTransferSessionCreate(
            allocator: kCFAllocatorDefault,
            pixelTransferSessionOut: &created
        )
        guard status == noErr, let created else { throw PreviewError.scaleFailed(status) }
        VTSessionSetProperty(
            created,
            key: kVTPixelTransferPropertyKey_DownsamplingMode,
            value: kVTDownsamplingMode_Average
        )
        transfer = created
        return created
    }
}
