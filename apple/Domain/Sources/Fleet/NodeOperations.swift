import AlondraContracts
import Foundation

public enum NodeMutationOutcome: Equatable, Sendable {
    case confirmed
    case rejected(String)
    case unknown
    case obsolete
}

public struct NodeFile: Equatable, Sendable, Identifiable {
    public let id: UUID
    public let name: String
    public let data: Data
    public let stored: DomainV2.AttachmentRef?

    public init(id: UUID = UUID(), name: String, data: Data, stored: DomainV2.AttachmentRef? = nil) {
        self.id = id
        self.name = name
        self.data = data
        self.stored = stored
    }
}

public struct NodeMessage: Equatable, Sendable {
    public let text: String
    public let files: [NodeFile]

    public init(text: String, files: [NodeFile]) {
        self.text = text
        self.files = files
    }
}

/// Composition root installs an identity-scoped adapter over CommandGate and the upload service.
/// The String is the Agent ID (the instance's session). Messages, closing and the orchestrator
/// controls are v2 client contracts (`message_agent`, `close_agent`, `*_orchestrator_run`).
/// The last String of every mutation is its command ID: a retry after an unknown outcome
/// reuses it and the server returns the first outcome.
public struct NodeOperations: Sendable {
    public var send: @Sendable (String, NodeMessage, String) async -> NodeMutationOutcome
    public var acknowledgeOrchestrator: @Sendable (String, String) async -> NodeMutationOutcome
    public var cancelOrchestrator: @Sendable (String, String) async -> NodeMutationOutcome
    public var retryOrchestrator: @Sendable (String, String) async -> NodeMutationOutcome
    public var close: @Sendable (String, String) async -> NodeMutationOutcome
    public var threads: @Sendable (String) async throws -> [NodeActivity]?

    public init(
        send: @escaping @Sendable (String, NodeMessage, String) async -> NodeMutationOutcome,
        close: @escaping @Sendable (String, String) async -> NodeMutationOutcome,
        threads: @escaping @Sendable (String) async throws -> [NodeActivity]?,
        retryOrchestrator: @escaping @Sendable (String, String) async -> NodeMutationOutcome = { _, _ in .obsolete },
        cancelOrchestrator: @escaping @Sendable (String, String) async -> NodeMutationOutcome = { _, _ in .obsolete },
        acknowledgeOrchestrator: @escaping @Sendable (String, String) async -> NodeMutationOutcome = { _, _ in .obsolete }
    ) {
        self.send = send
        self.close = close
        self.threads = threads
        self.retryOrchestrator = retryOrchestrator
        self.cancelOrchestrator = cancelOrchestrator
        self.acknowledgeOrchestrator = acknowledgeOrchestrator
    }
}

/// Only non-empty, trimmed text or files can be sent; the draft is cleared on acknowledgement.
public struct SendNodeMessageUseCase: Sendable {
    public init() {}

    public func prepare(text: String, files: [NodeFile]) -> NodeMessage? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty || !files.isEmpty else { return nil }
        return NodeMessage(text: trimmed, files: files)
    }
}

/// The endpoint may return no ledger on an old daemon; preserve its overview activity then.
public struct ResolveNodeThreadsUseCase: Sendable {
    public init() {}

    public func execute(asked: [NodeActivity]?, node: Node) -> [NodeActivity] {
        (asked ?? node.activity ?? node.toolThread).sorted { $0.since > $1.since }
    }
}
