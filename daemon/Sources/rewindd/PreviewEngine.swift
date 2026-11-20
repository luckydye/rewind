import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import ImageIO
import UniformTypeIdentifiers
import VideoToolbox

enum PreviewError: Error, CustomStringConvertible {
    case empty
    case noFormats
    case sessionFailed(OSStatus)
    case decodeFailed(OSStatus)
    case imageCreationFailed
    case encodingFailed

    var description: String {
        switch self {
        case .empty: return "the rolling buffer is empty"
        case .noFormats: return "the rolling buffer holds no frame to decode from"
        case .sessionFailed(let status): return "VTDecompressionSessionCreate failed (\(status))"
        case .decodeFailed(let status): return "VTDecompressionSessionDecodeFrame failed (\(status))"
        case .imageCreationFailed: return "could not turn the decoded buffer into an image"
        case .encodingFailed: return "could not encode the preview frame"
        }
    }
}

/// Decodes encoded samples from the ring on demand and hands back JPEGs.
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
    private var decodedImage: CGImage?
    private var decodedStatus: OSStatus = noErr

    deinit {
        if let session {
            VTDecompressionSessionInvalidate(session)
        }
    }

    /// `frames` must be ordered by presentation time and start at a keyframe.
    func jpeg(frames: [CMSampleBuffer], index: Int, width: Int) throws -> Data {
        guard !frames.isEmpty else { throw PreviewError.empty }
        let bounded = min(max(index, 0), frames.count - 1)

        lock.lock()
        defer { lock.unlock() }

        try decode(frames: frames, through: bounded)

        guard let image = decodedImage else { throw PreviewError.imageCreationFailed }
        let scaled = try resized(image, width: width)
        return try encodeJPEG(scaled)
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
            let usable = existing >= startIndex && existing < target
            if usable {
                startIndex = existing + 1
            } else {
                reset(format: description)
            }
        } else if self.decodedTime != nil {
            reset(format: description)
        }

        for index in startIndex...target {
            try decode(frame: frames[index])
        }

        if decodedImage == nil {
            throw PreviewError.imageCreationFailed
        }
    }

    private func decode(frame: CMSampleBuffer) throws {
        guard let session else { throw PreviewError.sessionFailed(-1) }
        decodedStatus = noErr
        let flags: VTDecodeFrameFlags = []
        var infoFlags = VTDecodeInfoFlags()

        let status = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: frame,
            flags: flags,
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
            decodedImage = nil
            return
        }
        var image: CGImage?
        let result = VTCreateCGImageFromCVPixelBuffer(imageBuffer, options: nil, imageOut: &image)
        decodedImage = result == noErr ? image : nil
    }

    private func reset(format: CMVideoFormatDescription) {
        if let session {
            VTDecompressionSessionInvalidate(session)
        }
        session = nil
        decodedTime = nil
        decodedImage = nil
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

    private func keyframeIndex(frames: [CMSampleBuffer], atOrBefore target: Int) -> Int {
        var found = 0
        for index in 0...target where frames[index].isKeyframe {
            found = index
        }
        return found
    }

    private func resized(_ image: CGImage, width: Int) throws -> CGImage {
        guard width > 0, width < image.width else { return image }
        let scale = Double(width) / Double(image.width)
        let height = max(1, Int((Double(image.height) * scale).rounded()))

        guard
            let context = CGContext(
                data: nil,
                width: width,
                height: height,
                bitsPerComponent: 8,
                bytesPerRow: 0,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue
                    | CGBitmapInfo.byteOrder32Little.rawValue
            )
        else {
            throw PreviewError.imageCreationFailed
        }
        context.interpolationQuality = .medium
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))

        guard let scaled = context.makeImage() else {
            throw PreviewError.imageCreationFailed
        }
        return scaled
    }

    private func encodeJPEG(_ image: CGImage) throws -> Data {
        let data = NSMutableData()
        guard
            let destination = CGImageDestinationCreateWithData(
                data as CFMutableData,
                UTType.jpeg.identifier as CFString,
                1,
                nil
            )
        else {
            throw PreviewError.encodingFailed
        }
        CGImageDestinationAddImage(
            destination,
            image,
            [kCGImageDestinationLossyCompressionQuality: 0.65] as CFDictionary
        )
        guard CGImageDestinationFinalize(destination) else {
            throw PreviewError.encodingFailed
        }
        return data as Data
    }
}
