import AlondraContracts
import Foundation

/// Where a root sits in the server's order, with its stable ID breaking ties.
public struct InboxPlace: Comparable, Hashable, Sendable {
    let order: UInt64
    let id: String

    public static func < (lhs: InboxPlace, rhs: InboxPlace) -> Bool {
        (lhs.order, lhs.id) < (rhs.order, rhs.id)
    }
}

extension DomainV2.Task: Identifiable {}
extension DomainV2.Request: Identifiable {}
extension DomainV2.PullRequest: Identifiable {}

extension DomainV2.InboxItem: Identifiable {
    public var place: InboxPlace { InboxPlace(order: order, id: id) }

    public var isPullRequest: Bool {
        if case .pullRequest = subject { true } else { false }
    }
}

/// The inbox as a ring in the server's order with a cursor on the card in front.
///
/// The cursor is a place in that order, not an id or an index: when the item under it goes
/// away (resolved here, answered in the terminal, withdrawn) the front becomes the first item
/// after that place, wrapping to the start. Items arriving or leaving elsewhere in the ring
/// never move it backwards.
public struct InboxRing: Sendable {
    public private(set) var cursor: InboxPlace?

    public init(cursor: InboxPlace? = nil) {
        self.cursor = cursor
    }

    public static func ordered(_ items: [DomainV2.InboxItem]) -> [DomainV2.InboxItem] {
        items.sorted { $0.place < $1.place }
    }

    public func frontIndex(in ring: [DomainV2.InboxItem]) -> Int {
        guard let cursor else { return 0 }
        return ring.firstIndex { $0.place >= cursor } ?? 0
    }

    public func arranged(_ ring: [DomainV2.InboxItem]) -> [DomainV2.InboxItem] {
        guard !ring.isEmpty else { return [] }
        let start = frontIndex(in: ring)
        return Array(ring[start...] + ring[..<start])
    }

    public mutating func move(from item: DomainV2.InboxItem? = nil, by step: Int, in ring: [DomainV2.InboxItem]) {
        guard !ring.isEmpty else { return }
        let origin = item.flatMap { item in ring.firstIndex { $0.id == item.id } } ?? frontIndex(in: ring)
        let count = ring.count
        cursor = ring[((origin + step) % count + count) % count].place
    }

    public mutating func focus(_ item: DomainV2.InboxItem) {
        cursor = item.place
    }

    /// Call before `item` leaves the ring. If it is the front card, the cursor stays on its
    /// place, so the next item in order takes the front once it is gone.
    public mutating func resolve(_ item: DomainV2.InboxItem, in ring: [DomainV2.InboxItem]) {
        guard !ring.isEmpty, ring[frontIndex(in: ring)].id == item.id else { return }
        cursor = item.place
    }
}
