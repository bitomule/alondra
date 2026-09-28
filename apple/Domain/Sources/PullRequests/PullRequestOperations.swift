import AlondraContracts
import Foundation

public struct PRDiffSnapshot: Equatable, Sendable {
    public let diff: String
    public let headSHA: String
    /// The Mac cut the diff to fit the encrypted relay; the rest is only on GitHub.
    public let truncated: Bool
    public init(diff: String, headSHA: String, truncated: Bool = false) {
        self.diff = diff; self.headSHA = headSHA; self.truncated = truncated
    }

    public init(_ diff: DomainV2.PullRequestDiff) {
        self.init(diff: diff.diff, headSHA: diff.head, truncated: diff.truncated)
    }
}

public enum PRMutationOutcome: Equatable, Sendable {
    case confirmed
    case rejected(String)
    case closed
    /// The response was lost; the caller keeps its draft and command ID to retry.
    case unknown
    case obsolete
}

/// A pull request card as the server grouped it: the PR, the presentation that carries its
/// merge and feedback actions, and the child questions that keep their own IDs.
public struct DomainPullRequestCard: Equatable, Sendable {
    public let root: DomainV2.InboxItem
    public let pullRequest: DomainV2.PullRequest
    public let presentation: DomainV2.InboxItem?
    public let questions: [DomainQuestion]
    public let information: [DomainV2.InboxItem]
    public let mergeAuthorizations: [DomainV2.MergeAuthorization]
    /// The head the agent presented. Diffs and feedback are bound to it, never to a newer one.
    public let presentedHead: String?
    /// What GitHub last reported: title, checks, mergeability, stack. Display only.
    public let observation: PullRequest?

    public init?(root: DomainV2.InboxItem, in workspace: DomainWorkspace) {
        guard case .pullRequest(let id) = root.subject, let pullRequest = workspace.pullRequest(id) else { return nil }
        self.root = root
        self.pullRequest = pullRequest
        presentation = root.children.last { $0.kind == "delivery" }
        questions = root.children.compactMap { DomainQuestion(item: $0, in: workspace) }
        information = root.children.filter { $0.kind == "information" }
        mergeAuthorizations = workspace.mergeAuthorizations(for: id)
        observation = workspace.runtime?.pullRequest(id)
        if case .delivery(let targets, _, _, _) = presentation.flatMap(workspace.interaction(of:))?.payload {
            presentedHead = targets.lazy.compactMap { target -> String? in
                if case let .pullRequest(prId, head) = target, prId == id { return head }
                return nil
            }.first
        } else {
            presentedHead = nil
        }
    }

    public var id: DomainV2.PrId { pullRequest.id }
    public var isOpen: Bool { pullRequest.state == .open }

    public func action(_ kind: DomainV2.ActionKind) -> DomainV2.AvailableAction? {
        presentation?.availableActions.first { $0.actionKind == kind }
    }

    public var presentationID: DomainV2.InteractionId? {
        guard case .interaction(let id) = presentation?.subject else { return nil }
        return id
    }

    public var canAuthorizeMerge: Bool { isOpen && action(.authorizeMerge)?.enabled == true }
    public var canSendFeedback: Bool { action(.submitFeedback)?.enabled == true }
    public var hasConflicts: Bool { observation?.hasConflicts == true }

    /// A recorded authorization waits for the agent; only an observed merge makes it merged.
    public var pendingAuthorization: DomainV2.MergeAuthorization? {
        mergeAuthorizations.last { $0.valid && $0.attempt != .confirmed && $0.attempt != .failed }
    }
}
