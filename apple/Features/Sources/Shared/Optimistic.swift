import AlondraContract
import AlondraFeed
import Foundation

/// What the person did that no server view shows yet. A screen draws the daemon's document through
/// it, so a tap takes effect at once, a view rendered before the daemon answered cannot bring the
/// item back, and an action the daemon did not apply puts the item back where it was. An answer the
/// daemon has not confirmed within `confirmationTimeout` is overdue: its item shows again, while the
/// answer goes on being delivered.
public struct Optimistic: Equatable, Sendable {
    private struct Sending: Equatable, Sendable { let target: String; let epoch: String }
    private struct Applied: Equatable, Sendable { let version: String?; let epoch: String }
    private var sending: [String: Sending] = [:]
    private var applied: [String: Applied] = [:]
    private var overdue: Set<String> = []

    /// How long an answer stays out of sight without the daemon confirming it.
    public static let confirmationTimeout = Duration.seconds(30)

    public init() {}

    public var isEmpty: Bool { sending.isEmpty && applied.isEmpty && overdue.isEmpty }

    public func hides(_ id: String?) -> Bool {
        guard let id, !overdue.contains(id) else { return false }
        return applied[id] != nil || isSending(id)
    }

    /// Whether an item stays out: answered here, or with an answer still on its way, until it is overdue.
    public func hides(_ id: String, undelivered: Set<String>) -> Bool {
        hides(id) || (undelivered.contains(id) && !overdue.contains(id))
    }

    public func isSending(_ id: String) -> Bool { sending.values.contains { $0.target == id } }

    public func isOverdue(_ id: String) -> Bool { overdue.contains(id) }

    public func target(of request: ActionRequest) -> String? {
        guard let pending = sending[request.deliveryCorrelationID], pending.epoch == request.dataEpoch else { return nil }
        return pending.target
    }

    public mutating func begin(_ id: String, _ request: ActionRequest, dataEpoch: String? = nil) {
        guard dataEpoch == nil || dataEpoch == request.dataEpoch else { return }
        sending[request.deliveryCorrelationID] = Sending(target: id, epoch: request.dataEpoch)
        overdue.remove(id)
    }

    /// The daemon has not confirmed what was sent for `id` in time: the item shows again. Returns
    /// whether it was still out of sight.
    @discardableResult
    public mutating func expire(_ id: String, undelivered: Set<String> = []) -> Bool {
        guard hides(id, undelivered: undelivered), applied[id] == nil else { return false }
        overdue.insert(id)
        return true
    }

    /// The answer for an overdue item is no longer on its way: the daemon's queue decides again.
    public mutating func forgetOverdue(keeping undelivered: Set<String>) {
        overdue = overdue.filter { isSending($0) || undelivered.contains($0) }
    }

    /// Ends the action `request` started. An applied action keeps its item hidden until a view newer
    /// than `shown` arrives; one the daemon refused or never got returns the item, which is returned.
    /// An overdue item already shows: applied, it leaves; refused, it stays where it is.
    @discardableResult
    public mutating func settle(_ request: ActionRequest, applied wasApplied: Bool, shown version: String?) -> String? {
        guard let pending = sending[request.deliveryCorrelationID], pending.epoch == request.dataEpoch else { return nil }
        sending.removeValue(forKey: request.deliveryCorrelationID)
        let wasOverdue = isSending(pending.target) ? false : overdue.remove(pending.target) != nil
        if wasApplied {
            applied[pending.target] = Applied(version: version, epoch: request.dataEpoch)
            return nil
        }
        return isSending(pending.target) || wasOverdue ? nil : pending.target
    }

    /// Keeps an item out after an action the daemon answered without applying it: only a newer
    /// view shows it again, where the daemon puts it, never back in front as if unanswered.
    public mutating func hold(_ id: String, shown version: String?, dataEpoch: String) {
        applied[id] = Applied(version: version, epoch: dataEpoch)
    }

    /// The daemon asks about the item again: it shows at once.
    public mutating func release(_ id: String) {
        applied.removeValue(forKey: id)
    }

    /// A view arrived: whatever it shows of an applied action is the daemon's word from now on.
    public mutating func reconcile(_ version: String?, dataEpoch: String? = nil) {
        if let dataEpoch { sending = sending.filter { $0.value.epoch == dataEpoch } }
        applied = applied.filter {
            $0.value.version == version && version != nil && (dataEpoch == nil || $0.value.epoch == dataEpoch)
        }
    }
}

public extension Optimistic {
    /// What the app says when an answer has not reached the Mac in time and its card waits again.
    static func notConfirmed(_ card: String?) -> String {
        guard let card, !card.isEmpty else { return String(localized: "Your Mac hasn’t confirmed an answer.") }
        return String(localized: "Your Mac hasn’t confirmed your answer to “\(card)”.")
    }
}
