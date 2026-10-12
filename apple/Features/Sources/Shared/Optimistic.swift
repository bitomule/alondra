import AlondraContract
import Foundation

/// What the person did that no server view shows yet. A screen draws the daemon's document through
/// it, so a tap takes effect at once, a view rendered before the daemon answered cannot bring the
/// item back, and an action the daemon did not apply puts the item back where it was.
public struct Optimistic: Equatable, Sendable {
    private struct Sending: Equatable, Sendable { let target: String; let epoch: String }
    private struct Applied: Equatable, Sendable { let version: String?; let epoch: String }
    private var sending: [String: Sending] = [:]
    private var applied: [String: Applied] = [:]

    public init() {}

    public var isEmpty: Bool { sending.isEmpty && applied.isEmpty }

    public func hides(_ id: String?) -> Bool {
        guard let id else { return false }
        return applied[id] != nil || sending.values.contains { $0.target == id }
    }

    public func isSending(_ id: String) -> Bool { sending.values.contains { $0.target == id } }

    public func target(of request: ActionRequest) -> String? {
        guard let pending = sending[request.commandId], pending.epoch == request.dataEpoch else { return nil }
        return pending.target
    }

    public mutating func begin(_ id: String, _ request: ActionRequest, dataEpoch: String? = nil) {
        guard dataEpoch == nil || dataEpoch == request.dataEpoch else { return }
        sending[request.commandId] = Sending(target: id, epoch: request.dataEpoch)
    }

    /// Ends the action `request` started. An applied action keeps its item hidden until a view newer
    /// than `shown` arrives; one the daemon refused or never got returns the item, which is returned.
    @discardableResult
    public mutating func settle(_ request: ActionRequest, applied wasApplied: Bool, shown version: String?) -> String? {
        guard let pending = sending[request.commandId], pending.epoch == request.dataEpoch else { return nil }
        sending.removeValue(forKey: request.commandId)
        if wasApplied {
            applied[pending.target] = Applied(version: version, epoch: request.dataEpoch)
            return nil
        }
        return isSending(pending.target) ? nil : pending.target
    }

    /// A view arrived: whatever it shows of an applied action is the daemon's word from now on.
    public mutating func reconcile(_ version: String?, dataEpoch: String? = nil) {
        if let dataEpoch { sending = sending.filter { $0.value.epoch == dataEpoch } }
        applied = applied.filter {
            $0.value.version == version && version != nil && (dataEpoch == nil || $0.value.epoch == dataEpoch)
        }
    }
}
