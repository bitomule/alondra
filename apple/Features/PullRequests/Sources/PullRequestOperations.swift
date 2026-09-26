import AlondraShared
import ComposableArchitecture
import Foundation

public struct PRDiffSnapshot: Equatable, Sendable {
    public let diff: String
    public let headSHA: String
    public init(diff: String, headSHA: String) { self.diff = diff; self.headSHA = headSHA }
}

public enum PRMutationOutcome: Equatable, Sendable {
    case confirmed
    case rejected(String)
    case closed
    case unknown(UUID)
    case obsolete

    public var uncertainID: UUID? {
        if case .unknown(let id) = self { return id }
        return nil
    }
}

public struct PRReviewAttachment: Equatable, Sendable, Identifiable {
    public let id: UUID
    public let name: String
    public let data: Data
    public let storedID: String?

    public init(id: UUID, name: String, data: Data, storedID: String? = nil) {
        self.id = id
        self.name = name
        self.data = data
        self.storedID = storedID
    }
}

public enum PRReviewKind: Sendable { case ask, requestChanges }

/// The Mac may have executed a POST before its encrypted response exceeded the relay cap.
/// The transport adapter throws this only after dispatch; CommandGate's `isUnknown`
/// closure must classify it as unknown and keep the reserved command in the journal.
public struct PRPostDispatchUncertain: Error, Sendable {
    public init() {}
}

public enum PRPostFailureClassifier {
    public static let isUnknown: @Sendable (any Error) -> Bool = { error in
        error is PRPostDispatchUncertain || error.isTransient
    }
}

/// H installs a credential-scoped adapter. Mutations must pass through CommandGate; the
/// adapter reserves before uploading attachments and never retries an unknown dispatch.
public struct PullRequestOperations: Sendable {
    public var diff: @Sendable (UInt64) async throws -> PRDiffSnapshot
    public var merge: @Sendable (UInt64, String) async -> PRMutationOutcome
    public var resolveConflicts: @Sendable (UInt64) async -> PRMutationOutcome
    public var setAside: @Sendable (UInt64) async -> PRMutationOutcome
    public var review: @Sendable (UInt64, String, [PRReviewAttachment], PRReviewKind, String) async -> PRMutationOutcome

    public init(
        diff: @escaping @Sendable (UInt64) async throws -> PRDiffSnapshot,
        merge: @escaping @Sendable (UInt64, String) async -> PRMutationOutcome,
        resolveConflicts: @escaping @Sendable (UInt64) async -> PRMutationOutcome,
        setAside: @escaping @Sendable (UInt64) async -> PRMutationOutcome,
        review: @escaping @Sendable (UInt64, String, [PRReviewAttachment], PRReviewKind, String) async -> PRMutationOutcome
    ) {
        self.diff = diff
        self.merge = merge
        self.resolveConflicts = resolveConflicts
        self.setAside = setAside
        self.review = review
    }
}

private enum PullRequestOperationsKey: DependencyKey {
    static let liveValue = PullRequestOperations(
        diff: { _ in preconditionFailure("Install PullRequestOperations at the app composition root") },
        merge: { _, _ in preconditionFailure("Install PullRequestOperations at the app composition root") },
        resolveConflicts: { _ in preconditionFailure("Install PullRequestOperations at the app composition root") },
        setAside: { _ in preconditionFailure("Install PullRequestOperations at the app composition root") },
        review: { _, _, _, _, _ in preconditionFailure("Install PullRequestOperations at the app composition root") }
    )
    static let testValue = liveValue
}

public extension DependencyValues {
    var pullRequestOperations: PullRequestOperations {
        get { self[PullRequestOperationsKey.self] }
        set { self[PullRequestOperationsKey.self] = newValue }
    }
}
