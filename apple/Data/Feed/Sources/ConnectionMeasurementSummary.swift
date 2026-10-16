import AlondraContract
import Foundation

/// Requests that took one path on one kind of network: how many, how fast the answered ones
/// were, and how many failed. The live-updates wait (`changes`, up to 50 s) and the time to open
/// the iroh connection are groups of their own, so they never hide how fast a request is.
public struct ConnectionMeasurementSummary: Equatable, Sendable, Identifiable {
    public enum Group: String, Comparable, Sendable {
        case requests
        case connecting
        case connectingInBackground
        case liveUpdates

        public static func < (lhs: Self, rhs: Self) -> Bool { lhs.order < rhs.order }
        private var order: Int { [.requests, .connecting, .connectingInBackground, .liveUpdates].firstIndex(of: self)! }

        init(kind: String) {
            switch kind {
            case "changes": self = .liveUpdates
            case ConnectionMeasurement.connectKind: self = .connecting
            case ConnectionMeasurement.backgroundConnectKind: self = .connectingInBackground
            default: self = .requests
            }
        }
    }

    public var route: ConnectionRoute?
    public var network: String
    public var group: Group
    public var count: Int
    public var failures: Int
    public var medianMs: Int?
    public var p90Ms: Int?

    public var id: String { "\(route?.rawValue ?? "none")/\(network)/\(group.rawValue)" }

    public init(route: ConnectionRoute?, network: String, group: Group = .requests, count: Int, failures: Int,
                medianMs: Int?, p90Ms: Int?) {
        self.route = route; self.network = network; self.group = group; self.count = count
        self.failures = failures; self.medianMs = medianMs; self.p90Ms = p90Ms
    }

    /// Cancelled requests say nothing about the connection and are left out.
    static func groups(_ measurements: [ConnectionMeasurement]) -> [ConnectionMeasurementSummary] {
        let counted = measurements.filter { $0.outcome != .cancelled }
        let grouped = Dictionary(grouping: counted) { GroupKey(route: $0.route, network: $0.network, group: Group(kind: $0.kind)) }
        return grouped.map { key, group in
            let answered = group.filter { $0.outcome == .ok || $0.outcome == .macError }.map(\.ms).sorted()
            return ConnectionMeasurementSummary(route: key.route, network: key.network, group: key.group, count: group.count,
                failures: group.count - answered.count, medianMs: percentile(answered, 50), p90Ms: percentile(answered, 90))
        }
        .sorted { ($0.group, -$0.count, $0.id) < ($1.group, -$1.count, $1.id) }
    }

    /// Nearest-rank percentile of sorted values.
    static func percentile(_ sorted: [Int], _ percent: Int) -> Int? {
        guard !sorted.isEmpty else { return nil }
        let rank = (percent * sorted.count + 99) / 100
        return sorted[max(rank, 1) - 1]
    }

    private struct GroupKey: Hashable {
        let route: ConnectionRoute?
        let network: String
        let group: Group
    }
}
