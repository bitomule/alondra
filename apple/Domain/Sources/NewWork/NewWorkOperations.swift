import AlondraContracts
import Foundation

/// The app composition root binds these operations to one credential-scoped service.
/// An attachment is retained in the feature until its upload succeeds; a failed upload
/// never sends a partial request.
public struct NewWorkOperations: Sendable {
    public var projects: @Sendable () async throws -> [String]
    public var options: @Sendable (String?) async throws -> OptionsCatalog
    public var issues: @Sendable (Bool) async throws -> IssueList
    public var captureSubmissionScope: @Sendable () async -> NewWorkSubmissionScope?

    public init(
        projects: @escaping @Sendable () async throws -> [String],
        options: @escaping @Sendable (String?) async throws -> OptionsCatalog,
        issues: @escaping @Sendable (Bool) async throws -> IssueList,
        captureSubmissionScope: @escaping @Sendable () async -> NewWorkSubmissionScope?
    ) {
        self.projects = projects
        self.options = options
        self.issues = issues
        self.captureSubmissionScope = captureSubmissionScope
    }
}

/// One immutable service/signing identity, captured before any attachment is uploaded.
/// Its closures must bind the same client and signer, never look up the current client
/// again. `create` must revalidate identity at its I/O boundary before dispatch.
public struct NewWorkSubmissionScope: Sendable {
    private let uploadOperation: @Sendable (NewRequestAttachment) async throws -> DomainV2.AttachmentRef
    private let identityIsCurrent: @Sendable () async -> Bool
    private let createOperation: @Sendable (NewRequestBody, String) async -> CreateRequestResult

    public init(
        upload: @escaping @Sendable (NewRequestAttachment) async throws -> DomainV2.AttachmentRef,
        isCurrent: @escaping @Sendable () async -> Bool,
        create: @escaping @Sendable (NewRequestBody, String) async -> CreateRequestResult
    ) {
        uploadOperation = upload
        identityIsCurrent = isCurrent
        createOperation = create
    }

    public func isCurrent() async -> Bool { await identityIsCurrent() }

    public func upload(_ attachment: NewRequestAttachment) async throws -> DomainV2.AttachmentRef {
        try Task.checkCancellation()
        guard await identityIsCurrent() else { throw CancellationError() }
        try Task.checkCancellation()
        return try await uploadOperation(attachment)
    }

    /// The command ID stays with the draft: a retry after an unknown outcome reuses it.
    public func create(_ body: NewRequestBody, commandID: String) async -> CreateRequestResult {
        guard !Task.isCancelled, await identityIsCurrent(), !Task.isCancelled else { return .cancelled }
        return await createOperation(body, commandID)
    }
}

/// Data is prepared by the platform file picker before entering TCA state.
public struct NewRequestAttachment: Identifiable, Equatable, Sendable {
    public let id: UUID
    public let name: String
    public let data: Data
    public let stored: DomainV2.AttachmentRef?

    public init(id: UUID, name: String, data: Data, stored: DomainV2.AttachmentRef? = nil) {
        self.id = id
        self.name = name
        self.data = data
        self.stored = stored
    }
}

/// Owns the submit transaction. There is no automatic retry of `create`: a lost
/// acknowledgement could mean the server already accepted the request.
public struct CreateRequestUseCase: Sendable {
    public var run: @Sendable (NewRequestBody, [NewRequestAttachment], String) async -> CreateRequestResult

    public init(operations: NewWorkOperations) {
        run = { body, attachments, commandID in
            do {
                guard let scope = await operations.captureSubmissionScope() else { return .cancelled }
                var body = body
                var stored: [DomainV2.AttachmentRef] = []
                for attachment in attachments {
                    try Task.checkCancellation()
                    guard await scope.isCurrent() else { return .cancelled }
                    stored.append(try await scope.upload(attachment))
                }
                try Task.checkCancellation()
                guard await scope.isCurrent() else { return .cancelled }
                try Task.checkCancellation()
                body.attachments = stored
                return await scope.create(body, commandID: commandID)
            } catch is CancellationError {
                return .cancelled
            } catch {
                return .failed(error.localizedDescription)
            }
        }
    }
}

public enum CreateRequestResult: Equatable, Sendable {
    case confirmed
    case failed(String)
    case uncertain(String)
    case cancelled
}
