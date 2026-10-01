import Foundation

/// One pairing approval pipeline per process. Each sheet owns a distinct scope so
/// dismissing another window cannot cancel or approve its pending invitation.
@MainActor
public final class PairingAttemptLease {
    public struct Ticket: Equatable, Sendable {
        fileprivate let id: UUID
        fileprivate let scope: UUID?
    }
    private var ticket: Ticket?
    public init() {}
    public var hasPending: Bool { ticket != nil }
    public func acquire(scope: UUID?) throws -> Ticket {
        guard ticket == nil else { throw CancellationError() }
        let next = Ticket(id: UUID(), scope: scope)
        ticket = next
        return next
    }
    public func current(scope: UUID?) -> Ticket? {
        guard ticket?.scope == scope else { return nil }
        return ticket
    }
    public func contains(_ value: Ticket) -> Bool { ticket == value }
    @discardableResult public func release(_ value: Ticket) -> Bool {
        guard ticket == value else { return false }
        ticket = nil
        return true
    }
    public func clear() { ticket = nil }
}
