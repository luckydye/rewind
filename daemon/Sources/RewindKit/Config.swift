import Foundation

public enum CaptureResolution: String, Codable, CaseIterable, Sendable {
    case automatic
    case best
    case nominal
}

public struct RecorderConfig: Codable, Equatable, Sendable {
    public var bufferSeconds: Double
    public var height: Int
    public var fps: Int
    public var bitrate: Int
    public var displayIndex: Int
    /// `nil` captures the whole display.
    public var applicationBundleID: String?
    public var captureResolution: CaptureResolution
    public var showsCursor: Bool

    public init(
        bufferSeconds: Double = 60,
        /// Output height in pixels; 0 captures at the display's native
        /// resolution. The display reports points, not pixels, so the native
        /// height of a 2x screen is twice what the UI suggests.
        height: Int = 0,
        fps: Int = 30,
        /// Bits per second; 0 derives a target from resolution and frame rate.
        bitrate: Int = 0,
        displayIndex: Int = 0,
        applicationBundleID: String? = nil,
        captureResolution: CaptureResolution = .automatic,
        showsCursor: Bool = true
    ) {
        self.bufferSeconds = bufferSeconds
        self.height = height
        self.fps = fps
        self.bitrate = bitrate
        self.displayIndex = displayIndex
        self.applicationBundleID = applicationBundleID
        self.captureResolution = captureResolution
        self.showsCursor = showsCursor
    }
}

public struct DisplayInfo: Codable, Equatable, Sendable {
    public var index: Int
    public var displayID: UInt32
    public var width: Int
    public var height: Int
    public var isMain: Bool

    public init(index: Int, displayID: UInt32, width: Int, height: Int, isMain: Bool) {
        self.index = index
        self.displayID = displayID
        self.width = width
        self.height = height
        self.isMain = isMain
    }

    public var name: String {
        "Display \(index) — \(width)×\(height)"
    }
}

public struct ApplicationInfo: Codable, Equatable, Sendable {
    public var name: String
    public var bundleIdentifier: String

    public init(name: String, bundleIdentifier: String) {
        self.name = name
        self.bundleIdentifier = bundleIdentifier
    }
}
