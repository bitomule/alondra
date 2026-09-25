import Foundation

/// Describes the effect for reconciliation. The payload stays with the caller until it has
/// been sent once; no closure or payload is retained for an automatic replay.
public enum CommandKind: Hashable, Sendable {
    case answer(UInt64)
    case moreContext(UInt64)
    case markRead(UInt64)
    case review(UInt64)
    case send(String)
    case replyToReport(UInt64)
    case newRequest
    case closeNode(String)
    case cancelTask(String)
    case reopenTask(String)
    case withdrawRequest(String)
    case archiveCompleted
    case mergePullRequest(UInt64)
    case resolveConflicts(UInt64)
    case requestChanges(UInt64)
    case askOnPullRequest(UInt64)
    case dismissPullRequest(UInt64)
    case task(String)


}

public struct CommandScope: Hashable, Sendable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String
    public let generation: UInt64

    public init(server: URL, deviceID: String, credentialRevision: String, generation: UInt64) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
        self.generation = generation
    }
}

public struct UncertainCommand: Identifiable, Equatable, Sendable {
    public let id: UUID
    public let scope: CommandScope
    public let kind: CommandKind
    public let itemID: String?

    public init(id: UUID, scope: CommandScope, kind: CommandKind, itemID: String?) {
        self.id = id
        self.scope = scope
        self.kind = kind
        self.itemID = itemID
    }
}

public enum CommandResult: Sendable {
    case confirmed
    case unknown(UncertainCommand)
    case rejected(any Error)
    case obsolete
}

/// One-shot submission. A lost response is kept as a fact to reconcile against a later
/// snapshot, never as an executable operation. Switching scope invalidates in-flight results.
public actor CommandGate {
    public private(set) var scope: CommandScope
    public private(set) var uncertain: [UncertainCommand] = []
    private var generation: UInt64 = 0

    public init(scope: CommandScope) { self.scope = scope }

    public func use(_ next: CommandScope) {
        guard next != scope else { return }
        guard next.generation > scope.generation else { return }
        scope = next
        generation &+= 1
        uncertain = []
    }

    public func dismiss(_ id: UUID) {
        uncertain.removeAll { $0.id == id }
    }

    public func submit(
        _ kind: CommandKind,
        itemID: String? = nil,
        in expectedScope: CommandScope,
        isUnknown: @Sendable (any Error) -> Bool,
        operation: @Sendable () async throws -> Void
    ) async -> CommandResult {
        guard expectedScope == scope else { return .obsolete }
        let submittedGeneration = generation
        do {
            try await operation()
            return submittedGeneration == generation ? .confirmed : .obsolete
        } catch {
            guard submittedGeneration == generation else { return .obsolete }
            guard isUnknown(error) else { return .rejected(error) }
            let command = UncertainCommand(id: UUID(), scope: expectedScope, kind: kind, itemID: itemID)
            uncertain.append(command)
            return .unknown(command)
        }
    }
}
