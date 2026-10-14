import Foundation
#if os(iOS)
import CoreTelephony
#endif

/// The cellular radio technology data goes over, such as `NR` (5G) or `LTE`; nil off iPhone.
public enum RadioAccess {
    #if os(iOS)
    nonisolated(unsafe) private static let info = CTTelephonyNetworkInfo()
    #endif

    public static var current: String? {
        #if os(iOS)
        guard let service = info.dataServiceIdentifier,
              let technology = info.serviceCurrentRadioAccessTechnology?[service] else { return nil }
        return technology.replacingOccurrences(of: "CTRadioAccessTechnology", with: "")
        #else
        nil
        #endif
    }
}
