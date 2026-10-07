import AlondraContract
import Foundation

public extension ConnectionRoute {
    var title: LocalizedStringResource {
        switch self {
        case .thisMac: "On this Mac"
        case .localNetwork: "On the local network"
        case .tailscale: "Via Tailscale"
        case .relay: "Via the relay"
        }
    }

    var systemImage: String {
        switch self {
        case .thisMac: "desktopcomputer"
        case .localNetwork: "wifi"
        case .tailscale: "point.3.connected.trianglepath.dotted"
        case .relay: "cloud"
        }
    }
}
