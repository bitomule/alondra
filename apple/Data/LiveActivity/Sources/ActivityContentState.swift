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

/// The Live Activity's content. ActivityKit encodes and decodes it with a default `JSONEncoder`
/// and `JSONDecoder`, so every field is a plain number or string.
public struct ActivityContentState: Codable, Hashable, Sendable {
    public var reports: [ActivityReport]

    public init(reports: [ActivityReport]) { self.reports = reports }
}
