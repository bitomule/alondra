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
/// `uncertain` means the response was lost: the caller keeps its draft and command ID, and a
/// retry with that ID returns the first outcome instead of applying the command twice.
public enum DomainCommandResult: Equatable, Sendable {
    case recorded(DomainV2.CommandOutcome)
    case rejected(DomainCommandRejection)
    case uncertain
    case obsolete
}

/// The single v2 boundary every feature uses: one projection stream and one command path.
public struct DomainWorkspaceClient: Sendable {
    public var observe: @Sendable () async -> AsyncStream<DomainWorkspaceUpdate>
    /// The String is the command ID. The first call builds the envelope from the workspace;
    /// a retry with the same ID after an unknown outcome resends that same envelope.
    public var perform: @Sendable (DomainIntent, DomainWorkspace, [DomainV2.AvailableAction], String) async -> DomainCommandResult
    public var taskActions: @Sendable (DomainV2.TaskId, DomainV2.DataEpoch) async throws -> [DomainV2.AvailableAction]
    public var pullRequestDiff: @Sendable (DomainV2.PrId, String) async throws -> DomainV2.PullRequestDiff
    public var documentContent: @Sendable (DomainV2.DocumentRevisionId, String) async throws -> String
    /// The brief a Task was launched with and each state it went through, for its detail.
    public var taskHistory: @Sendable (DomainV2.TaskId) async throws -> DomainV2.TaskHistory
    /// A screen appeared: the client loads what it shows. Calling it again is harmless.
    public var open: @Sendable (DomainScreen) async -> Void

    public init(
        observe: @escaping @Sendable () async -> AsyncStream<DomainWorkspaceUpdate>,
        perform: @escaping @Sendable (DomainIntent, DomainWorkspace, [DomainV2.AvailableAction], String) async -> DomainCommandResult,
        taskActions: @escaping @Sendable (DomainV2.TaskId, DomainV2.DataEpoch) async throws -> [DomainV2.AvailableAction],
        pullRequestDiff: @escaping @Sendable (DomainV2.PrId, String) async throws -> DomainV2.PullRequestDiff,
        documentContent: @escaping @Sendable (DomainV2.DocumentRevisionId, String) async throws -> String,
        taskHistory: @escaping @Sendable (DomainV2.TaskId) async throws -> DomainV2.TaskHistory = { _ in DomainV2.TaskHistory() },
        open: @escaping @Sendable (DomainScreen) async -> Void = { _ in }
    ) {
        self.observe = observe
        self.perform = perform
        self.taskActions = taskActions
        self.pullRequestDiff = pullRequestDiff
        self.documentContent = documentContent
        self.taskHistory = taskHistory
        self.open = open
    }

    /// Neither observes nor sends anything: an unconfigured feature stays inert.
    public static let inert = DomainWorkspaceClient(
        observe: { AsyncStream { $0.finish() } },
        perform: { _, _, _, _ in .obsolete },
        taskActions: { _, _ in throw CancellationError() },
        pullRequestDiff: { _, _ in throw CancellationError() },
        documentContent: { _, _ in throw CancellationError() }
    )
}
