import AlondraShared
import ComposableArchitecture
import Foundation

public enum NodeMutationOutcome: Equatable, Sendable {
    case confirmed
    case rejected(String)
    case unknown(UUID)
    case obsolete

    public var uncertainID: UUID? {
        if case .unknown(let id) = self { return id }
        return nil
    }
}

public struct NodeFile: Equatable, Sendable, Identifiable {
    public let id: UUID
    public let name: String
    public let data: Data

    public init(id: UUID = UUID(), name: String, data: Data) {
        self.id = id
        self.name = name
        self.data = data
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
/// The adapter must reserve the send command before uploading files or dispatching transport.
public struct NodeOperations: Sendable {
    public var send: @Sendable (String, NodeMessage) async -> NodeMutationOutcome
    public var close: @Sendable (String) async -> NodeMutationOutcome
    public var threads: @Sendable (String) async throws -> [NodeActivity]?

    public init(
        send: @escaping @Sendable (String, NodeMessage) async -> NodeMutationOutcome,
        close: @escaping @Sendable (String) async -> NodeMutationOutcome,
        threads: @escaping @Sendable (String) async throws -> [NodeActivity]?
    ) {
        self.send = send
        self.close = close
        self.threads = threads
    }
}

private enum NodeOperationsKey: DependencyKey {
    static let liveValue = NodeOperations(
        send: { _, _ in preconditionFailure("Install NodeOperations at the app composition root") },
        close: { _ in preconditionFailure("Install NodeOperations at the app composition root") },
        threads: { _ in preconditionFailure("Install NodeOperations at the app composition root") }
    )
    static let testValue = liveValue
}

public extension DependencyValues {
    var nodeOperations: NodeOperations {
        get { self[NodeOperationsKey.self] }
        set { self[NodeOperationsKey.self] = newValue }
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
