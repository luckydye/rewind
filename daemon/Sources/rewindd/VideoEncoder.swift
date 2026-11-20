import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

enum EncoderError: Error, CustomStringConvertible {
    case sessionCreationFailed(OSStatus)
    case propertyFailed(key: String, status: OSStatus)

    var description: String {
        switch self {
        case .sessionCreationFailed(let status):
            return "VTCompressionSessionCreate failed (\(status))"
        case .propertyFailed(let key, let status):
            return "failed to set \(key) (\(status))"
        }
    }
}

/// Hardware HEVC encoder. Frames are handed over as buffers owned by the capture
/// pipeline; nothing is copied through the CPU.
final class VideoEncoder {
    private var session: VTCompressionSession?
    private let onSample: (CMSampleBuffer) -> Void
    private let stats: CaptureStats

    private var lastKeyframeTime: CMTime?
    private let keyframeInterval: Double

    private(set) var usesHardwareAcceleration = false
    private(set) var width = 0
    private(set) var height = 0

    init(
        stats: CaptureStats,
        keyframeInterval: Double = 1,
        onSample: @escaping (CMSampleBuffer) -> Void
    ) {
        self.stats = stats
        self.keyframeInterval = keyframeInterval
        self.onSample = onSample
    }

    deinit {
        finish()
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
        // Idle frames are never encoded, so the encoder's own frame-count based
        // GOP drifts away from wall time and a "last 5 seconds" export can end
        // up cutting at a keyframe from a minute ago. Keyframes are therefore
        // forced from the presentation timestamp instead.
        lastKeyframeTime = nil
        // A quality target *and* an average bitrate, but deliberately no hard
        // DataRateLimits ceiling: VT undershoots its average target when the
        // two are close, which reads as over-compression. The ring's byte limit
        // is what bounds memory instead.
        try set(kVTCompressionPropertyKey_Quality, 0.85 as CFNumber)
        try set(kVTCompressionPropertyKey_AverageBitRate, bitrate as CFNumber)

        usesHardwareAcceleration = readBool(kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder)
        VTCompressionSessionPrepareToEncodeFrames(session)
    }

    func encode(_ pixelBuffer: CVPixelBuffer, presentationTime: CMTime) {
        guard let session else { return }

        let elapsed = lastKeyframeTime.map { presentationTime.seconds - $0.seconds }
            ?? .infinity
        let forceKeyframe = elapsed >= keyframeInterval
        if forceKeyframe {
            lastKeyframeTime = presentationTime
        }

        let status = VTCompressionSessionEncodeFrame(
            session,
            imageBuffer: pixelBuffer,
            presentationTimeStamp: presentationTime,
            duration: .invalid,
            frameProperties: forceKeyframe
                ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary
                : nil,
            sourceFrameRefcon: nil,
            infoFlagsOut: nil
        )
        if status != noErr {
            stats.recordError()
        }
    }

    /// Drains any in-flight frames before the session goes away, so the bare
    /// pointer handed to VT as refcon can never outlive this object.
    func finish() {
        guard let session else { return }
        self.session = nil
        VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
        VTCompressionSessionInvalidate(session)
    }

    fileprivate func handleOutput(_ sample: CMSampleBuffer) {
        stats.recordEncoded()
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
