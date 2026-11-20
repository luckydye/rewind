import CoreMedia
import Foundation

enum FrameRingError: Error, CustomStringConvertible {
    case empty
    case noKeyframeAlignment
    case noFormatDescription

    var description: String {
        switch self {
        case .empty: return "the rolling buffer is empty"
        case .noKeyframeAlignment: return "the rolling buffer holds no keyframe to cut from"
        case .noFormatDescription: return "the keyframe carries no format description"
        }
    }
}

struct RingSnapshot {
    let frames: [CMSampleBuffer]
    let duration: TimeInterval
    let bytes: Int

    var bitrate: Double {
        duration > 0 ? Double(bytes) * 8 / duration : 0
    }
}

/// Holds encoded frames for a rolling window. Access is serialised because the
/// capture queue appends while the main queue snapshots for statistics/export.
final class FrameRing {
    private let lock = NSLock()
    private var frames: [CMSampleBuffer] = []
    private var bytes = 0

    let capacity: TimeInterval
    let byteLimit: Int

    init(capacity: TimeInterval, byteLimit: Int = 512 * 1024 * 1024) {
        self.capacity = capacity
        self.byteLimit = byteLimit
    }

    func append(_ sample: CMSampleBuffer) {
        let size = CMSampleBufferGetTotalSampleSize(sample)
        lock.lock()
        defer { lock.unlock() }

        frames.append(sample)
        bytes += size
        evict()
    }

    /// Returns the frames between `from` and `to` seconds before now, cut at the
    /// last keyframe before `from` so the clip decodes without re-encoding.
    func snapshot(from secondsAgo: TimeInterval, to endSecondsAgo: TimeInterval) throws -> RingSnapshot {
        lock.lock()
        defer { lock.unlock() }

        guard let last = frames.last else { throw FrameRingError.empty }
        let now = last.presentationTimeStamp.seconds
        let windowStart = now - secondsAgo
        let windowEnd = now - endSecondsAgo

        // Find where the window begins by timestamp, then walk back to the
        // nearest keyframe. Scanning every frame for keyframes here would cost
        // one CoreMedia attachment lookup per frame on every preview request.
        let boundary = frames.firstIndex {
            $0.presentationTimeStamp.seconds >= windowStart
        } ?? frames.count - 1

        var start: Int?
        for index in stride(from: boundary, through: 0, by: -1) where frames[index].isKeyframe {
            start = index
            break
        }
        guard let start else { throw FrameRingError.noKeyframeAlignment }

        let end = frames.lastIndex {
            $0.presentationTimeStamp.seconds <= windowEnd
        } ?? start
        guard end >= start else { throw FrameRingError.empty }

        let slice = Array(frames[start...end])
        let duration = slice.last!.presentationTimeStamp.seconds
            - slice.first!.presentationTimeStamp.seconds

        return RingSnapshot(
            frames: slice,
            duration: duration,
            bytes: slice.reduce(0) { $0 + CMSampleBufferGetTotalSampleSize($1) }
        )
    }

    func snapshot(last seconds: TimeInterval) throws -> RingSnapshot {
        try snapshot(from: seconds, to: 0)
    }

    func statistics() -> (bytes: Int, duration: TimeInterval, frames: Int) {
        lock.lock()
        defer { lock.unlock() }
        guard let first = frames.first, let last = frames.last else {
            return (0, 0, 0)
        }
        return (bytes, last.presentationTimeStamp.seconds - first.presentationTimeStamp.seconds, frames.count)
    }

    private func evict() {
        while frames.count > 1 {
            let span = frames[frames.count - 1].presentationTimeStamp.seconds
                - frames[0].presentationTimeStamp.seconds
            let overTime = span > capacity
            let overBytes = bytes > byteLimit
            guard overTime || overBytes else { return }

            bytes -= CMSampleBufferGetTotalSampleSize(frames[0])
            frames.removeFirst()
        }
    }
}

extension CMSampleBuffer {
    var isKeyframe: Bool {
        guard
            let attachments = CMSampleBufferGetSampleAttachmentsArray(self, createIfNecessary: false)
                as? [[CFString: Any]],
            let first = attachments.first
        else {
            return true
        }
        return (first[kCMSampleAttachmentKey_NotSync] as? Bool) != true
    }
}
