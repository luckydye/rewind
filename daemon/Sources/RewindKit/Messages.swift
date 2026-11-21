import Foundation

/// A slice of the rolling buffer, measured in seconds before now.
public struct ClipRange: Codable, Sendable {
    public var from: Double
    public var to: Double

    public init(from: Double, to: Double) {
        self.from = from
        self.to = to
    }

    public var duration: Double { from - to }
}

public enum Command: String, Codable, Sendable {
    case status
    case start
    case stop
    case setConfig
    case exportClip
    case exportFrame
    case listDisplays
    case listApplications
    case previewFrame
    case thumbnails
    case edgeFrames
    case estimate
    case copyFrame
    case copyClip
    case openUI
    case quit
}

/// A rectangle of the capture, as fractions of its width and height.
public struct Crop: Codable, Sendable {
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double

    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }
}

/// What `exportClip` and `copyClip` write. `exportFrame` always writes PNG.
public enum ExportFormat: String, Codable, Sendable {
    case mp4
    case gif

    public var fileExtension: String { rawValue }
}

/// GIFs are sampled down from the capture: full rate and native size make
/// files of hundreds of megabytes that nothing will play smoothly.
public enum GifDefaults {
    public static let fps: Double = 15
    public static let maxWidth = 1280
}

/// Flat request shape on purpose: the UI is written in Rust and mirrors these
/// structs by hand, so no associated values or custom coding.
public struct Request: Codable, Sendable {
    public var command: Command
    public var config: RecorderConfig?
    public var seconds: Double?
    public var path: String?
    public var width: Int?
    public var count: Int?
    public var range: ClipRange?
    /// Absolute timestamp (seconds) that "now" refers to. The overlay pins it
    /// when it opens; without it, times are measured from the newest frame,
    /// which keeps moving because capture continues while the overlay is open.
    public var anchor: Double?
    /// Raw RGBA annotation layer (u32 width, u32 height, then pixels) for the
    /// daemon to composite into a copied frame or an exported clip.
    public var overlayPath: String?
    /// The part of the capture to keep, if it was cropped.
    public var crop: Crop?
    /// Clip format; MP4 when absent. For GIF, `width` is the maximum width.
    public var format: ExportFormat?
    /// GIF frame rate.
    public var fps: Double?

    public init(
        command: Command,
        config: RecorderConfig? = nil,
        seconds: Double? = nil,
        path: String? = nil,
        width: Int? = nil,
        count: Int? = nil,
        range: ClipRange? = nil,
        anchor: Double? = nil,
        overlayPath: String? = nil,
        crop: Crop? = nil,
        format: ExportFormat? = nil,
        fps: Double? = nil
    ) {
        self.command = command
        self.config = config
        self.seconds = seconds
        self.path = path
        self.width = width
        self.count = count
        self.range = range
        self.anchor = anchor
        self.overlayPath = overlayPath
        self.crop = crop
        self.format = format
        self.fps = fps
    }
}

public struct State: Codable, Sendable {
    public var recording: Bool
    public var config: RecorderConfig
    public var display: DisplayInfo?
    public var width: Int
    public var height: Int
    public var bufferSeconds: Double
    public var availableSeconds: Double
    public var bufferBytes: Int
    public var bitrate: Double
    public var framesCaptured: Int
    public var framesEncoded: Int
    public var droppedFrames: Int
    public var idleNotices: Int
    public var onBattery: Bool
    public var lastError: String?
    /// Timestamp of the newest buffered frame; the overlay's anchor.
    public var newestTimestamp: Double?

    public init(
        recording: Bool,
        config: RecorderConfig,
        display: DisplayInfo?,
        width: Int,
        height: Int,
        bufferSeconds: Double,
        availableSeconds: Double,
        bufferBytes: Int,
        bitrate: Double,
        framesCaptured: Int,
        framesEncoded: Int,
        droppedFrames: Int,
        idleNotices: Int,
        onBattery: Bool,
        lastError: String?,
        newestTimestamp: Double?
    ) {
        self.recording = recording
        self.config = config
        self.display = display
        self.width = width
        self.height = height
        self.bufferSeconds = bufferSeconds
        self.availableSeconds = availableSeconds
        self.bufferBytes = bufferBytes
        self.bitrate = bitrate
        self.framesCaptured = framesCaptured
        self.framesEncoded = framesEncoded
        self.droppedFrames = droppedFrames
        self.idleNotices = idleNotices
        self.onBattery = onBattery
        self.lastError = lastError
        self.newestTimestamp = newestTimestamp
    }
}

public struct Clip: Codable, Sendable {
    public var path: String
    public var duration: Double
    public var bytes: Int
    public var frames: Int
    public var width: Int
    public var height: Int

    public init(
        path: String,
        duration: Double,
        bytes: Int,
        frames: Int,
        width: Int,
        height: Int
    ) {
        self.path = path
        self.duration = duration
        self.bytes = bytes
        self.frames = frames
        self.width = width
        self.height = height
    }
}

public struct Response: Codable, Sendable {
    public var ok: Bool
    public var error: String?
    public var state: State?
    public var clip: Clip?
    public var displays: [DisplayInfo]?
    public var applications: [ApplicationInfo]?
    /// Exact size of the encoded frames a range would export.
    public var estimatedBytes: Int?

    public init(
        ok: Bool,
        error: String? = nil,
        state: State? = nil,
        clip: Clip? = nil,
        displays: [DisplayInfo]? = nil,
        applications: [ApplicationInfo]? = nil,
        estimatedBytes: Int? = nil
    ) {
        self.ok = ok
        self.error = error
        self.state = state
        self.clip = clip
        self.displays = displays
        self.applications = applications
        self.estimatedBytes = estimatedBytes
    }

    public static func failure(_ message: String) -> Response {
        Response(ok: false, error: message)
    }
}
