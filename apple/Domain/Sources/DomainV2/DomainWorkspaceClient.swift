import AlondraContracts
import Foundation

public struct DomainWorkspaceUpdate: Equatable, Sendable {
    public let identity: TriageIdentity
    public let workspace: DomainWorkspace?
    public let connection: TriageConnection

    public init(identity: TriageIdentity, workspace: DomainWorkspace?, connection: TriageConnection) {
        self.identity = identity
        self.workspace = workspace
        self.connection = connection
    }
}

public struct DomainCommandRejection: Error, Equatable, Sendable {
    public let code: String?
    public let message: String

    public init(code: String?, message: String) {
        self.code = code
        self.message = message
    }

    public var errorCode: DomainV2.ErrorCode? { code.flatMap(DomainV2.ErrorCode.init(rawValue:)) }
}

/// A recorded command is a decision the server stored, never proof of an external effect.
public enum DomainCommandResult: Equatable, Sendable {
    case recorded(DomainV2.CommandOutcome)
    case rejected(DomainCommandRejection)
    case uncertain(UUID)
    case obsolete
}

/// The single v2 boundary every feature uses: one projection stream and one command path.
public struct DomainWorkspaceClient: Sendable {
    public var observe: @Sendable () async -> AsyncStream<DomainWorkspaceUpdate>
    public var perform: @Sendable (DomainIntent, DomainWorkspace, [DomainV2.AvailableAction]) async -> DomainCommandResult
    public var taskActions: @Sendable (DomainV2.TaskId, DomainV2.DataEpoch) async throws -> [DomainV2.AvailableAction]
    public var pullRequestDiff: @Sendable (DomainV2.PrId, String) async throws -> DomainV2.PullRequestDiff
    public var documentContent: @Sendable (DomainV2.DocumentRevisionId, String) async throws -> String

    public init(
        observe: @escaping @Sendable () async -> AsyncStream<DomainWorkspaceUpdate>,
        perform: @escaping @Sendable (DomainIntent, DomainWorkspace, [DomainV2.AvailableAction]) async -> DomainCommandResult,
        taskActions: @escaping @Sendable (DomainV2.TaskId, DomainV2.DataEpoch) async throws -> [DomainV2.AvailableAction],
        pullRequestDiff: @escaping @Sendable (DomainV2.PrId, String) async throws -> DomainV2.PullRequestDiff,
        documentContent: @escaping @Sendable (DomainV2.DocumentRevisionId, String) async throws -> String
    ) {
        self.observe = observe
        self.perform = perform
        self.taskActions = taskActions
        self.pullRequestDiff = pullRequestDiff
        self.documentContent = documentContent
    }

    /// Neither observes nor sends anything: an unconfigured feature stays inert.
    public static let inert = DomainWorkspaceClient(
        observe: { AsyncStream { $0.finish() } },
        perform: { _, _, _ in .obsolete },
        taskActions: { _, _ in throw CancellationError() },
        pullRequestDiff: { _, _ in throw CancellationError() },
        documentContent: { _, _ in throw CancellationError() }
    )
}
