import Foundation
import IOKit.ps

public enum PowerSource {
    public static var isOnBattery: Bool {
        guard let snapshot = IOPSCopyPowerSourcesInfo()?.takeRetainedValue() else { return false }
        guard let type = IOPSGetProvidingPowerSourceType(snapshot)?.takeUnretainedValue() else {
            return false
        }
        return (type as String) == kIOPSBatteryPowerValue
    }
}
