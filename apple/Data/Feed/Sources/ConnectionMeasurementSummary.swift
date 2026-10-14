import AlondraContract
import Foundation

/// Requests that took one path on one kind of network: how many, how fast the answered ones
/// were, and how many failed. The live-updates wait (`changes`, up to 50 s) is its own group so
/// it never hides how fast a normal request is.
public struct ConnectionMeasurementSummary: Equatable, Sendable, Identifiable {
    public var route: ConnectionRoute?
    public var network: String
    public var waits: Bool
    public var count: Int
    public var failures: Int
    public var medianMs: Int?
    public var p90Ms: Int?

    public var id: String { "\(route?.rawValue ?? "none")/\(network)/\(waits)" }

    public init(route: ConnectionRoute?, network: String, waits: Bool, count: Int, failures: Int, medianMs: Int?, p90Ms: Int?) {
        self.route = route; self.network = network; self.waits = waits; self.count = count
        self.failures = failures; self.medianMs = medianMs; self.p90Ms = p90Ms
    }

    /// Cancelled requests say nothing about the connection and are left out.
    static func groups(_ measurements: [ConnectionMeasurement]) -> [ConnectionMeasurementSummary] {
        let counted = measurements.filter { $0.outcome != .cancelled }
        let grouped = Dictionary(grouping: counted) { GroupKey(route: $0.route, network: $0.network, waits: $0.kind == "changes") }
        return grouped.map { key, group in
            let answered = group.filter { $0.outcome == .ok || $0.outcome == .macError }.map(\.ms).sorted()
            return ConnectionMeasurementSummary(route: key.route, network: key.network, waits: key.waits, count: group.count,
                failures: group.count - answered.count, medianMs: percentile(answered, 50), p90Ms: percentile(answered, 90))
        }
        .sorted { ($0.waits ? 1 : 0, -$0.count, $0.id) < ($1.waits ? 1 : 0, -$1.count, $1.id) }
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
        let waits: Bool
    }
}
