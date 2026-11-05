import AlondraTransport
import Foundation

/// The three numbers the Live Activity shows, for one Mac or summed over all of them.
public struct ActivityCounts: Codable, Hashable, Sendable {
    /// Cards in Waiting for You, the number on the app icon.
    public var inbox: Int
    /// Tasks waiting for an answer from you.
    public var waiting: Int
    /// Tasks whose node is working.
    public var working: Int

    public init(inbox: Int, waiting: Int, working: Int) {
        self.inbox = inbox; self.waiting = waiting; self.working = working
    }

    public static let zero = ActivityCounts(inbox: 0, waiting: 0, working: 0)

    public var isZero: Bool { self == .zero }

    public static func + (lhs: Self, rhs: Self) -> Self {
        ActivityCounts(inbox: lhs.inbox + rhs.inbox, waiting: lhs.waiting + rhs.waiting, working: lhs.working + rhs.working)
    }
}

/// What one Mac last said, and when.
public struct ActivityReport: Codable, Hashable, Sendable {
    public var macID: String
    public var macName: String?
    public var counts: ActivityCounts
    public var measuredAtMs: Int64

    public init(macID: String, macName: String?, counts: ActivityCounts, measuredAt: Date) {
        self.macID = macID; self.macName = macName; self.counts = counts
        measuredAtMs = Int64((measuredAt.timeIntervalSince1970 * 1000).rounded())
    }

    public var measuredAt: Date { Date(timeIntervalSince1970: Double(measuredAtMs) / 1000) }

    enum CodingKeys: String, CodingKey {
        case macID = "mac_id", macName = "mac_name", counts, measuredAtMs = "measured_at_ms"
    }
}

/// The Live Activity's content, from the app or from a push of the relay. ActivityKit encodes and
/// decodes it with a default `JSONEncoder` and `JSONDecoder`, and a push leaves out what only the
/// app writes, so every field may be missing.
public struct ActivityContentState: Codable, Hashable, Sendable {
    /// What the app measured itself, in the clear because it never leaves the phone.
    public var reports: [ActivityReport]
    /// Each Mac's latest report as the relay holds it, sealed for this phone.
    public var macs: [Envelope]
    /// The Macs that did not fit in the push.
    public var omitted: Int

    public init(reports: [ActivityReport] = [], macs: [Envelope] = [], omitted: Int = 0) {
        self.reports = reports; self.macs = macs; self.omitted = omitted
    }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        reports = try values.decodeIfPresent([ActivityReport].self, forKey: .reports) ?? []
        macs = try values.decodeIfPresent([Envelope].self, forKey: .macs) ?? []
        omitted = try values.decodeIfPresent(Int.self, forKey: .omitted) ?? 0
    }

    /// The state that fits what ActivityKit takes, leaving out the oldest sealed reports: the
    /// relay sends the newest first. The attributes share the 4 KB.
    public func fitted(toBytes limit: Int = 3_800) -> Self {
        var fitted = self
        while !fitted.macs.isEmpty, (try? JSONEncoder().encode(fitted).count) ?? .max > limit {
            fitted.macs.removeLast()
            fitted.omitted += 1
        }
        return fitted
    }

    /// The newest word of every Mac, from the app's reports and the ones opened from `macs`.
    public func merged(with opened: [ActivityReport]) -> [ActivityReport] {
        var newest: [String: ActivityReport] = [:]
        for report in reports + opened where newest[report.macID].map({ $0.measuredAtMs < report.measuredAtMs }) ?? true {
            newest[report.macID] = report
        }
        return newest.keys.sorted().compactMap { newest[$0] }
    }
}
