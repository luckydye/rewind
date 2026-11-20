import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

enum EncoderError: Error, CustomStringConvertible {
    case sessionCreationFailed(OSStatus)
    case propertyFailed(key: String, status: OSStatus)
    case noHardwareEncoder

    var description: String {
        switch self {
        case .sessionCreationFailed(let status):
            return "VTCompressionSessionCreate failed (\(status))"
        case .propertyFailed(let key, let status):
            return "failed to set \(key) (\(status))"
        case .noHardwareEncoder:
            return "this machine exposes no hardware HEVC encoder"
        }
    }
}

/// Hardware HEVC encoder. Frames are handed over as IOSurfaces owned by the
/// capture pipeline; nothing is copied through the CPU.
final class VideoEncoder {
    private var session: VTCompressionSession?
    private let onSample: (CMSampleBuffer) -> Void

    private(set) var usesHardwareAcceleration = false
    private(set) var width = 0
    private(set) var height = 0
    private(set) var encodedFrames = 0
    private(set) var encodeErrors = 0

    init(onSample: @escaping (CMSampleBuffer) -> Void) {
        self.onSample = onSample
    }

    deinit {
        if let session {
            VTCompressionSessionInvalidate(session)
        }
    }

    func prepare(width: Int, height: Int, fps: Int, bitrate: Int) throws {
        assert(width % 2 == 0 && height % 2 == 0, "HEVC needs even dimensions")
        self.width = width
        self.height = height

        var sessionOut: VTCompressionSession?
        let encoderSpecification: [CFString: Any] = [
            kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: true
        ]
        let imageAttributes: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
            kCVPixelBufferMetalCompatibilityKey: true
        ]

        let status = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            width: Int32(width),
            height: Int32(height),
            codecType: kCMVideoCodecType_HEVC,
            encoderSpecification: encoderSpecification as CFDictionary,
            imageBufferAttributes: imageAttributes as CFDictionary,
            compressedDataAllocator: kCFAllocatorDefault,
            outputCallback: encoderOutputCallback,
            refcon: Unmanaged.passUnretained(self).toOpaque(),
            compressionSessionOut: &sessionOut
        )

        guard status == noErr, let session = sessionOut else {
            throw EncoderError.sessionCreationFailed(status)
        }
        self.session = session

        try set(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue)
        try set(kVTCompressionPropertyKey_ExpectedFrameRate, fps as CFNumber)
        try set(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse)
        try set(kVTCompressionPropertyKey_AllowTemporalCompression, kCFBooleanTrue)
        try set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main_AutoLevel)
        // A one second GOP bounds how much of the requested clip we can lose to
        // keyframe alignment when exporting.
        try set(kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, fps as CFNumber)
        try set(kVTCompressionPropertyKey_MaxKeyFrameInterval, fps as CFNumber)
        // Ceiling rather than a target: static screens cost nothing extra.
        try set(kVTCompressionPropertyKey_DataRateLimits, [bitrate / 8, 1] as CFArray)

        usesHardwareAcceleration = readBool(kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder)
        VTCompressionSessionPrepareToEncodeFrames(session)
    }

    func encode(_ pixelBuffer: CVPixelBuffer, presentationTime: CMTime) {
        guard let session else { return }
        let status = VTCompressionSessionEncodeFrame(
            session,
            imageBuffer: pixelBuffer,
            presentationTimeStamp: presentationTime,
            duration: .invalid,
            frameProperties: nil,
            sourceFrameRefcon: nil,
            infoFlagsOut: nil
        )
        if status != noErr {
            encodeErrors += 1
        }
    }

    func handleOutput(_ sample: CMSampleBuffer) {
        encodedFrames += 1
        onSample(sample)
    }

    private func set(_ key: CFString, _ value: CFTypeRef) throws {
        guard let session else { return }
        let status = VTSessionSetProperty(session, key: key, value: value)
        guard status == noErr else {
            throw EncoderError.propertyFailed(key: key as String, status: status)
        }
    }

    private func readBool(_ key: CFString) -> Bool {
        guard let session else { return false }
        let valueOut = UnsafeMutablePointer<CFTypeRef?>.allocate(capacity: 1)
        defer { valueOut.deallocate() }
        valueOut.initialize(to: nil)
        let status = VTSessionCopyProperty(session, key: key, allocator: nil, valueOut: valueOut)
        guard status == noErr else { return false }
        return (valueOut.pointee as? Bool) ?? false
    }
}

private let encoderOutputCallback: VTCompressionOutputCallback = {
    refCon, _, status, _, sampleBuffer in
    guard status == noErr, let refCon, let sampleBuffer else { return }
    let encoder = Unmanaged<VideoEncoder>.fromOpaque(refCon).takeUnretainedValue()
    encoder.handleOutput(sampleBuffer)
}
