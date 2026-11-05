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
        /// The relay's reports arrived and none could be opened here.
        case openTheApp
        /// More Macs report than fit in one push.
        case macsNotCounted(Int)
        case allCaughtUp
    }

    public let totals: ActivityCounts
    public let updatedAt: Date?
    public let notice: Notice?

    public init(totals: ActivityCounts, updatedAt: Date?, notice: Notice?) {
        self.totals = totals; self.updatedAt = updatedAt; self.notice = notice
    }

    /// Whether there are numbers at all: with no report opened, zero would be a lie.
    public var isKnown: Bool { notice != .openTheApp }

    /// Whether the numbers are old enough that they should look it.
    public var isOutdated: Bool {
        if case .noNewsFromAnyMac = notice { true } else { false }
    }

    /// A report stamped ahead of `now` (a Mac whose clock runs fast) counts as made `now`.
    public init(_ state: ActivityContentState, opened: [ActivityReport] = [], now: Date, stale: Bool) {
        let reports = state.merged(with: opened)
        totals = reports.reduce(.zero) { $0 + $1.counts }
        let times = reports.map { min($0.measuredAt, now) }
        updatedAt = times.max()
        let quiet = zip(reports, times)
            .filter { now.timeIntervalSince($0.1) > Self.quietAfter }
            .min { $0.1 < $1.1 }
        let sealed = Set(state.macs.map(\.sender))
        let uncounted = state.omitted - state.reports.filter { !sealed.contains($0.macID) }.count
        if reports.isEmpty, !state.macs.isEmpty {
            notice = .openTheApp
        } else if stale, let updatedAt {
            notice = .noNewsFromAnyMac(since: updatedAt)
        } else if let (report, since) = quiet {
            notice = .noNewsFromMac(name: report.macName, since: since)
        } else if uncounted > 0 {
            notice = .macsNotCounted(uncounted)
        } else if totals.isZero {
            notice = .allCaughtUp
        } else {
            notice = nil
        }
    }
}
