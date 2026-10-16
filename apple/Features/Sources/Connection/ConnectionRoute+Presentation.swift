import AlondraContract
import Foundation

public extension ConnectionRoute {
    var title: LocalizedStringResource {
        switch self {
        case .thisMac: "On this Mac"
        case .localNetwork: "On the local network"
        case .tailscale: "Via Tailscale"
        case .irohRelay: "Via the relay"
        case .direct: "Direct connection"
        }
    }

    var systemImage: String {
        switch self {
        case .thisMac: "desktopcomputer"
        case .localNetwork: "wifi"
        case .tailscale: "point.3.connected.trianglepath.dotted"
        case .irohRelay: "cloud"
        case .direct: "bolt.horizontal"
        }
    }
}
