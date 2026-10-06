import Foundation

/// Which notices the user was already told about, kept across launches so a reconnect never
/// repeats one. A new daemon database (another epoch) starts over.
public struct LocalNoticeLedger: Codable, Equatable, Sendable {
    static let limit = 500
    public var epoch: String?
    public var seen: Set<Int64>

    public init(epoch: String? = nil, seen: Set<Int64> = []) {
        self.epoch = epoch; self.seen = seen
    }

    mutating func remember(_ ids: some Sequence<Int64>) {
        seen.formUnion(ids)
        if seen.count > Self.limit {
            seen = Set(seen.sorted().suffix(Self.limit))
        }
    }
}
