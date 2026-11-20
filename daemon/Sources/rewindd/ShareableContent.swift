import CoreGraphics
import Foundation
import RewindKit
import ScreenCaptureKit

enum ShareableContent {
    /// `NSScreen.safeAreaInsets` is the only API that accounts for the camera
    /// housing; `visibleFrame` only knows about the menu bar.
    static func screen(for displayID: CGDirectDisplayID) -> NSScreen? {
        let key = NSDeviceDescriptionKey("NSScreenNumber")
        return NSScreen.screens.first {
            ($0.deviceDescription[key] as? NSNumber)?.uint32Value == displayID
        }
    }

    static func displays() async throws -> [DisplayInfo] {
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: true
        )
        return content.displays.enumerated().map { index, display in
            let screen = Self.screen(for: display.displayID)
            return DisplayInfo(
                index: index,
                displayID: display.displayID,
                width: CGDisplayPixelsWide(display.displayID),
                height: CGDisplayPixelsHigh(display.displayID),
                isMain: CGDisplayIsMain(display.displayID) != 0,
                safeAreaTop: Double(screen?.safeAreaInsets.top ?? 0),
                screenX: Double(screen?.frame.origin.x ?? 0),
                screenY: Double(screen?.frame.origin.y ?? 0),
                screenWidth: Double(screen?.frame.width ?? 0),
                screenHeight: Double(screen?.frame.height ?? 0)
            )
        }
    }

    static func applications() async throws -> [ApplicationInfo] {
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: true
        )
        var seen = Set<String>()
        var result: [ApplicationInfo] = []

        for application in content.applications.sorted(by: {
            $0.applicationName.localizedCaseInsensitiveCompare($1.applicationName) == .orderedAscending
        }) {
            let bundleID = application.bundleIdentifier
            guard !bundleID.isEmpty, !seen.contains(bundleID) else { continue }
            seen.insert(bundleID)
            result.append(ApplicationInfo(name: application.applicationName, bundleIdentifier: bundleID))
        }
        return result
    }
}
