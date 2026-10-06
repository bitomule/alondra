import AlondraContract
import Foundation

/// What the person did that no server view shows yet. A screen draws the daemon's document through
/// it, so a tap takes effect at once, a view rendered before the daemon answered cannot bring the
/// item back, and an action the daemon did not apply puts the item back where it was.
public struct Optimistic: Equatable, Sendable {
    private var sending: [String: String] = [:]
    private var applied: [String: String?] = [:]

    public init() {}

    public var isEmpty: Bool { sending.isEmpty && applied.isEmpty }

    public func hides(_ id: String?) -> Bool {
        guard let id else { return false }
        return applied[id] != nil || sending.values.contains(id)
    }

    public func isSending(_ id: String) -> Bool { sending.values.contains(id) }

    public func target(of request: ActionRequest) -> String? { sending[request.commandId] }

    public mutating func begin(_ id: String, _ request: ActionRequest) {
        sending[request.commandId] = id
    }

    /// Ends the action `request` started. An applied action keeps its item hidden until a view newer
    /// than `shown` arrives; one the daemon refused or never got returns the item, which is returned.
    @discardableResult
    public mutating func settle(_ request: ActionRequest, applied wasApplied: Bool, shown version: String?) -> String? {
        guard let id = sending.removeValue(forKey: request.commandId) else { return nil }
        if wasApplied {
            applied.updateValue(version, forKey: id)
            return nil
        }
        return isSending(id) ? nil : id
    }

    /// A view arrived: whatever it shows of an applied action is the daemon's word from now on.
    public mutating func reconcile(_ version: String?) {
        applied = applied.filter { $0.value == version && version != nil }
    }
}
