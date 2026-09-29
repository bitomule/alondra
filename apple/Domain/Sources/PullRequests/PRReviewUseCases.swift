import AlondraContracts
import Foundation

public enum PRReviewDecision: Equatable, Sendable {
    case ready(String)
    case empty
    case closed
    case outdatedDiff
}

/// The diff is the authority for line anchors. A comment on an old revision must never be
/// sent as if it still referred to the current diff.
public struct PreparePRReviewUseCase: Sendable {
    public init() {}

    public func execute(comments: [String: String], note: String, files: [DiffFile]?,
                        isOpen: Bool, commentsAreCurrent: Bool) -> PRReviewDecision {
        guard isOpen else { return .closed }
        let trimmed = note.trimmingCharacters(in: .whitespacesAndNewlines)
        let valid = Set((files ?? []).flatMap(\.lines).compactMap(\.anchor))
        // Sending only a subset would silently drop user feedback. The caller keeps the
        // draft and asks for a fresh review when the diff changed.
        guard commentsAreCurrent, comments.keys.allSatisfy(valid.contains) else { return .outdatedDiff }
        let normalized = comments.compactMapValues { text -> String? in
            let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
            return value.isEmpty ? nil : value
        }
        guard !normalized.isEmpty || !trimmed.isEmpty else { return .empty }
        let ordered = ReviewExport.ordered(normalized, in: files ?? [])
        return .ready(ReviewExport.text(comments: ordered, general: trimmed))
    }
}

public enum SubmitPRReviewResult: Equatable, Sendable {
    case diffChanged(PRDiffSnapshot)
    case finished(DomainCommandResult)
}

/// Reloads the presented head's diff immediately before sending, so feedback written on one
/// diff is never sent against another. The server checks the presented head again.
public struct SubmitPRReviewUseCase: Sendable {
    private let client: DomainWorkspaceClient

    public init(client: DomainWorkspaceClient) { self.client = client }

    public func execute(card: DomainPullRequestCard, text: String, attachments: [DomainV2.AttachmentRef] = [],
                        expected: PRDiffSnapshot?, in workspace: DomainWorkspace,
                        commandID: String) async throws -> SubmitPRReviewResult {
        guard let presentation = card.presentationID else {
            return .finished(.rejected(DomainCommandRejection(code: DomainV2.ErrorCode.invalidActionForKind.rawValue,
                                                              message: "No presentation to review")))
        }
        if let expected {
            let current = PRDiffSnapshot(try await client.pullRequestDiff(card.id, expected.headSHA))
            try Task.checkCancellation()
            guard current == expected else { return .diffChanged(current) }
        }
        return .finished(await client.perform(.submitFeedback(presentation, body: text, attachments: attachments),
                                              workspace, [], commandID))
    }
}
