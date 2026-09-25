import Foundation

/// A push may be delivered before the overview containing its card. Wait for an overview
/// accepted after the push before deciding that the item is no longer pending.
public struct TriageRouteWait: Equatable, Sendable {
    public enum Resolution: Equatable, Sendable {
        case waiting
        case open(String)
        case unavailable
    }

    public let itemID: String
    public let afterSnapshotRevision: UInt64

    public init(itemID: String, afterSnapshotRevision: UInt64) {
        self.itemID = itemID
        self.afterSnapshotRevision = afterSnapshotRevision
    }

    public func resolve(snapshotRevision: UInt64, targets: [String: String]) -> Resolution {
        if let target = targets[itemID] { return .open(target) }
        return snapshotRevision > afterSnapshotRevision ? .unavailable : .waiting
    }
}
