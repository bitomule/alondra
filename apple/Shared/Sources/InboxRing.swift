import Foundation

/// Where an item sits in arrival order: questions first by `askedAt`, then reports by
/// `createdAt`, with the server id breaking ties.
public struct InboxPlace: Comparable, Hashable, Sendable {
    let group: Int
    let date: Int64
    let serial: UInt64

    public static func < (lhs: InboxPlace, rhs: InboxPlace) -> Bool {
        (lhs.group, lhs.date, lhs.serial) < (rhs.group, rhs.date, rhs.serial)
    }
}

extension InboxItem {
    public var place: InboxPlace {
        switch self {
        case .question(let q): InboxPlace(group: 0, date: q.askedAt, serial: q.id)
        case .report(let r): InboxPlace(group: 1, date: r.createdAt, serial: r.id)
        }
    }
}

/// The inbox as a ring in arrival order with a cursor on the card in front.
///
/// The cursor is a place in that order, not an id or an index: when the item under it goes
/// away (resolved here, answered in the terminal, withdrawn, orphaned) the front becomes the
/// first item after that place, wrapping to the start. Items arriving or leaving elsewhere in
/// the ring never move it backwards.
public struct InboxRing: Sendable {
    public private(set) var cursor: InboxPlace?

    public init(cursor: InboxPlace? = nil) {
        self.cursor = cursor
    }

    public static func ordered(_ items: [InboxItem]) -> [InboxItem] {
        items.sorted { $0.place < $1.place }
    }

    public func frontIndex(in ring: [InboxItem]) -> Int {
        guard let cursor else { return 0 }
        return ring.firstIndex { $0.place >= cursor } ?? 0
    }

    public func arranged(_ ring: [InboxItem]) -> [InboxItem] {
        guard !ring.isEmpty else { return [] }
        let start = frontIndex(in: ring)
        return Array(ring[start...] + ring[..<start])
    }

    public mutating func move(from item: InboxItem? = nil, by step: Int, in ring: [InboxItem]) {
        guard !ring.isEmpty else { return }
        let origin = item.flatMap { item in ring.firstIndex { $0.id == item.id } } ?? frontIndex(in: ring)
        let count = ring.count
        cursor = ring[((origin + step) % count + count) % count].place
    }

    public mutating func focus(_ item: InboxItem) {
        cursor = item.place
    }

    /// Call before `item` leaves the ring. If it is the front card, the cursor stays on its
    /// place, so the next item in order takes the front once it is gone.
    public mutating func resolve(_ item: InboxItem, in ring: [InboxItem]) {
        guard !ring.isEmpty, ring[frontIndex(in: ring)].id == item.id else { return }
        cursor = item.place
    }
}
