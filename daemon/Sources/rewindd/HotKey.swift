import Carbon.HIToolbox
import Foundation

/// Global hotkey. Carbon's `RegisterEventHotKey` is used rather than a
/// `CGEventTap` because it needs no accessibility permission and cannot be
/// blocked by another app's tap.
///
/// This lives in the daemon because GPUI has no API for global shortcuts, and
/// the daemon is the process that exists before the UI does.
final class HotKeyCenter {
    static let shared = HotKeyCenter()

    var action: (() -> Void)?

    private var hotKey: EventHotKeyRef?
    private var handler: EventHandlerRef?

    private static let signature: OSType = 0x5257_4431  // 'RWD1'

    @discardableResult
    func register() -> Bool {
        guard hotKey == nil else { return true }

        var eventType = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )
        let installed = InstallEventHandler(
            GetApplicationEventTarget(),
            { _, _, _ in
                HotKeyCenter.shared.action?()
                return noErr
            },
            1,
            &eventType,
            nil,
            &handler
        )
        guard installed == noErr else {
            FileHandle.standardError.write(Data("rewindd: hotkey handler failed\n".utf8))
            return false
        }

        let identifier = EventHotKeyID(signature: Self.signature, id: 1)
        let registered = RegisterEventHotKey(
            UInt32(kVK_ANSI_S),
            UInt32(cmdKey | shiftKey),
            identifier,
            GetApplicationEventTarget(),
            0,
            &hotKey
        )
        guard registered == noErr else {
            FileHandle.standardError.write(
                Data("rewindd: Cmd+Shift+S is taken (status \(registered))\n".utf8)
            )
            return false
        }
        return true
    }
}
