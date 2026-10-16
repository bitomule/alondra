import AlondraFeed
import Foundation

extension ConnectionMeasurementSummary {
    var title: LocalizedStringResource { "\(path) · \(networkTitle)" }

    var path: LocalizedStringResource {
        switch group {
        case .liveUpdates: return "Live updates · \(routeTitle)"
        case .connecting: return "Opening the connection"
        case .connectingInBackground: return "Opening the connection in the background"
        case .requests: return routeTitle
        }
    }

    private var routeTitle: LocalizedStringResource {
        switch route {
        case .irohRelay: return "Live relay"
        case let route?: return route.title
        case nil: return "No answer"
        }
    }

    var networkTitle: LocalizedStringResource {
        switch network {
        case "wifi": "Wi-Fi"
        case "cellular": "Mobile data"
        case "wired": "Ethernet"
        case "offline": "Offline"
        default: "Other network"
        }
    }

    var requests: LocalizedStringResource {
        if group == .connecting || group == .connectingInBackground { return "\(count) connections" }
        return "\(count) requests"
    }

    var failed: LocalizedStringResource? { failures > 0 ? "\(failures) failed" : nil }

    var timing: LocalizedStringResource? {
        guard let medianMs, let p90Ms else { return nil }
        return "\(medianMs) ms · p90 \(p90Ms) ms"
    }
}
