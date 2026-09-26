import Foundation

public struct DeliveryStatus: Equatable, Sendable {
    public enum Phase: Equatable, Sendable, Codable {
        case unconfirmed
        case feedbackMessageRejected(Int)
        case feedbackReadRejected(Int)
        case feedbackNeedsDraft
    }
    public struct Operation: Identifiable, Equatable, Sendable {
        public let id: UUID
        public let phase: Phase
        public let firstObservedUncertainAt: Date
        public let acknowledged: Bool
        public init(id: UUID, phase: Phase, firstObservedUncertainAt: Date, acknowledged: Bool) {
            self.id = id; self.phase = phase; self.firstObservedUncertainAt = firstObservedUncertainAt
            self.acknowledged = acknowledged
        }
    }
    public let scope: CommandScope
    public let operations: [Operation]
    /// Local warning metadata could not be read or saved; never implies delivery failure.
    public let persistenceError: String?
    public init(scope: CommandScope, operations: [Operation] = [], persistenceError: String? = nil) {
        self.scope = scope; self.operations = operations; self.persistenceError = persistenceError
    }
}
