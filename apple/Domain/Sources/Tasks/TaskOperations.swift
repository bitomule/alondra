import AlondraContracts
import Foundation

/// A mutation may have reached the server even when its acknowledgement was lost.
public enum TaskMutationOutcome: Equatable, Sendable {
    case confirmed
    case unknown(UUID)
    case rejected(String)
    case obsolete
}

public extension TaskMutationOutcome {
    var uncertainID: UUID? {
        if case .unknown(let id) = self { return id }
        return nil
    }
}

/// Application boundary. The composition root supplies operations backed by CommandGate and
/// the current credential scope; features never hold a URLSession or AlondraClient.
public struct TaskOperations: Sendable {
    public var withdraw: @Sendable (WorkRequest) async -> TaskMutationOutcome
    public var cancel: @Sendable (WorkTask) async -> TaskMutationOutcome
    public var reopen: @Sendable (WorkTask) async -> TaskMutationOutcome
    public var archiveCompleted: @Sendable () async -> TaskMutationOutcome
    public var details: @Sendable (WorkTask) async -> TaskInFull?

    public init(
        withdraw: @escaping @Sendable (WorkRequest) async -> TaskMutationOutcome = { _ in .rejected("Request withdrawal is unavailable") },
        cancel: @escaping @Sendable (WorkTask) async -> TaskMutationOutcome,
        reopen: @escaping @Sendable (WorkTask) async -> TaskMutationOutcome,
        archiveCompleted: @escaping @Sendable () async -> TaskMutationOutcome,
        details: @escaping @Sendable (WorkTask) async -> TaskInFull?
    ) {
        self.withdraw = withdraw
        self.cancel = cancel
        self.reopen = reopen
        self.archiveCompleted = archiveCompleted
        self.details = details
    }
}
