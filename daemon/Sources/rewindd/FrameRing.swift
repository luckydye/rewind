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

    /// Returns the frames covering the last `seconds`, cut at the last keyframe
    /// before the window start so the clip can be decoded without re-encoding.
    func snapshot(last seconds: TimeInterval) throws -> RingSnapshot {
        lock.lock()
        defer { lock.unlock() }

        guard let last = frames.last else { throw FrameRingError.empty }
        let windowStart = last.presentationTimeStamp.seconds - seconds

        var startIndex: Int?
        for (index, frame) in frames.enumerated() where frame.isKeyframe {
            if frame.presentationTimeStamp.seconds <= windowStart {
                startIndex = index
            } else {
                break
            }
        }

        // Nothing predates the window (buffer shorter than the clip) — start at
        // the earliest keyframe we have.
        if startIndex == nil {
            startIndex = frames.firstIndex(where: \.isKeyframe)
        }
        guard let start = startIndex else { throw FrameRingError.noKeyframeAlignment }

        let slice = Array(frames[start...])
        let duration = slice.last!.presentationTimeStamp.seconds
            - slice.first!.presentationTimeStamp.seconds

        return RingSnapshot(
            frames: slice,
            duration: duration,
            bytes: slice.reduce(0) { $0 + CMSampleBufferGetTotalSampleSize($1) }
        )
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
