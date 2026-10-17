import AlondraContract
import AlondraTransport
import Foundation

/// The requests sent from this device that the daemon's queue does not show yet. Sending puts one
/// on top at once; it stays until the daemon's queue shows it, which then takes its place, or until
/// a view newer than the one shown when the daemon took it leaves it out. A view drawn before the
/// daemon answered never takes one away, a failed one stays until it is sent again or discarded,
/// and the same request is never shown twice: the daemon names it by the operation that carried it
/// (`op_…_<id>`), whose last part is the id it was sent with.
public struct PendingRequests: Equatable, Sendable {
    public enum Phase: Equatable, Sendable {
        /// Waiting for what it needs before it can go: the options from the daemon, or its attachments.
        case waiting(Wait)
        case sending
        /// The daemon has not confirmed it within `Optimistic.confirmationTimeout`; it goes on retrying.
        case unconfirmed
        case failed(String)
        /// The daemon took it while `shown` was on screen.
        case applied(shown: String?)
    }

    public enum Wait: Equatable, Sendable { case options, attachments }

    public struct Entry: Equatable, Sendable, Identifiable {
        public let id: String
        public let text: String
        public var phase: Phase

        public var isFailed: Bool { if case .failed = phase { true } else { false } }
    }

    public private(set) var entries: [Entry] = []

    public init() {}

    public subscript(id: String) -> Entry? { entries.first { $0.id == id } }

    public mutating func add(_ id: String, text: String, phase: Phase) {
        guard self[id] == nil else { return }
        entries.insert(Entry(id: id, text: text, phase: phase), at: 0)
    }

    public mutating func set(_ id: String, _ phase: Phase) {
        guard let index = entries.firstIndex(where: { $0.id == id }) else { return }
        entries[index].phase = phase
    }

    /// Only a send still waiting for its answer becomes unconfirmed.
    public mutating func expire(_ id: String) {
        guard self[id]?.phase == .sending else { return }
        set(id, .unconfirmed)
    }

    public mutating func discard(_ id: String) {
        entries.removeAll { $0.id == id }
    }

    /// A view of the daemon's queue arrived: what it shows is the daemon's from now on, and what it
    /// leaves out after taking it has left the queue.
    public mutating func reconcile(shown rows: [AlondraContract.Row], version: String?) {
        let ids = Self.ids(rows)
        entries.removeAll { entry in
            if ids.contains(entry.id) { return true }
            guard case .applied(let shown) = entry.phase else { return false }
            return version != nil && version != shown
        }
    }

    /// The local requests the daemon's rows do not show yet, newest first.
    public func unshown(_ rows: [AlondraContract.Row]) -> [Entry] {
        let shown = Self.ids(rows)
        return entries.filter { !shown.contains($0.id) }
    }

    private static func ids(_ rows: [AlondraContract.Row]) -> Set<String> {
        Set(rows.map { ActionOperationID.nonce(from: $0.id) })
    }
}
