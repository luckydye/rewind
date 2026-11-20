import Foundation
import ServiceManagement

/// `SMAppService.mainApp` only means something inside an app bundle with a
/// bundle identifier, so an unbundled development build reports nothing.
enum LoginItem {
    static var isSupported: Bool {
        guard let identifier = Bundle.main.bundleIdentifier, !identifier.isEmpty else {
            return false
        }
        return Bundle.main.bundlePath.hasSuffix(".app")
    }

    static var isEnabled: Bool {
        isSupported && SMAppService.mainApp.status == .enabled
    }

    static func set(_ enabled: Bool) throws {
        precondition(isSupported, "login items require an app bundle")
        if enabled {
            try SMAppService.mainApp.register()
        } else {
            try SMAppService.mainApp.unregister()
        }
    }
}
