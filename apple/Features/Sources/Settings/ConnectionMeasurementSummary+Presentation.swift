import AlondraFeed
import Foundation

extension ConnectionMeasurementSummary {
    var title: LocalizedStringResource { "\(path) · \(networkTitle)" }

    var path: LocalizedStringResource {
        if waits { return "Live updates" }
        return route?.title ?? "No answer"
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

    var requests: LocalizedStringResource { "\(count) requests" }

    var failed: LocalizedStringResource? { failures > 0 ? "\(failures) failed" : nil }

    var timing: LocalizedStringResource? {
        guard let medianMs, let p90Ms else { return nil }
        return "\(medianMs) ms · p90 \(p90Ms) ms"
    }
}
