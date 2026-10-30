import Foundation

/// What the Live Activity draws: the numbers summed over every Mac, and the one line it adds
/// under them when something needs saying.
public struct ActivitySummary: Equatable, Sendable {
    /// A Mac that has said nothing for this long is named in the line under the numbers.
    public static let quietAfter: TimeInterval = 30 * 60

    public enum Notice: Equatable, Sendable {
        /// Nothing has arrived from any Mac: the Live Activity is stale.
        case noNewsFromAnyMac(since: Date)
        case noNewsFromMac(name: String?, since: Date)
        case allCaughtUp
    }

    public let totals: ActivityCounts
    public let updatedAt: Date?
    public let notice: Notice?

    public init(totals: ActivityCounts, updatedAt: Date?, notice: Notice?) {
        self.totals = totals; self.updatedAt = updatedAt; self.notice = notice
    }

    /// A report stamped ahead of `now` (a Mac whose clock runs fast) counts as made `now`.
    public init(_ state: ActivityContentState, now: Date, stale: Bool) {
        let reports = state.reports
        totals = reports.reduce(.zero) { $0 + $1.counts }
        let times = reports.map { min($0.measuredAt, now) }
        updatedAt = times.max()
        let quiet = zip(reports, times)
            .filter { now.timeIntervalSince($0.1) > Self.quietAfter }
            .min { $0.1 < $1.1 }
        if stale, let updatedAt {
            notice = .noNewsFromAnyMac(since: updatedAt)
        } else if let (report, since) = quiet {
            notice = .noNewsFromMac(name: report.macName, since: since)
        } else if totals.isZero {
            notice = .allCaughtUp
        } else {
            notice = nil
        }
    }
}
