import CoreGraphics
import Foundation
import RewindKit
import ScreenCaptureKit

enum ShareableContent {
    /// `NSScreen.safeAreaInsets` is the only API that accounts for the camera
    /// housing; `visibleFrame` only knows about the menu bar.
    static func safeAreaTop(for displayID: CGDirectDisplayID) -> Double {
        let key = NSDeviceDescriptionKey("NSScreenNumber")
        for screen in NSScreen.screens
        where (screen.deviceDescription[key] as? NSNumber)?.uint32Value == displayID {
            return Double(screen.safeAreaInsets.top)
        }
        return 0
    }

    static func displays() async throws -> [DisplayInfo] {
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: true
        )
        return content.displays.enumerated().map { index, display in
            DisplayInfo(
                index: index,
                displayID: display.displayID,
                width: CGDisplayPixelsWide(display.displayID),
                height: CGDisplayPixelsHigh(display.displayID),
                isMain: CGDisplayIsMain(display.displayID) != 0,
                safeAreaTop: safeAreaTop(for: display.displayID)
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
