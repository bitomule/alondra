import Foundation

public extension TrustedDevice.Origin {
    var title: LocalizedStringResource {
        switch self {
        case .mac: "On this Mac"
        case .direct: "On the local network"
        case .relay: "Via the relay"
        }
    }
}
