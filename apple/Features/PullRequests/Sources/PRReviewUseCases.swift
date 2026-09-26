import AlondraShared
import Foundation

public enum PRReviewDecision: Equatable, Sendable {
    case ready(String)
    case empty
    case closed
    case outdatedDiff
    case textRequired
}

/// The diff is the authority for line anchors. A comment on an old revision must never be
/// sent as if it still referred to the current diff.
public struct PreparePRReviewUseCase: Sendable {
    public init() {}

    public func execute(
        comments: [String: String], note: String, attachments: [PRReviewAttachment],
        files: [DiffFile]?, isOpen: Bool, commentsAreCurrent: Bool
    ) -> PRReviewDecision {
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
        guard !normalized.isEmpty || !trimmed.isEmpty else {
            return attachments.isEmpty ? .empty : .textRequired
        }
        let ordered = ReviewExport.ordered(normalized, in: files ?? [])
        return .ready(ReviewExport.text(comments: ordered, general: trimmed))
    }
}

/// A snapshot can refresh while a merge dialog is open. Validate again at confirmation.
public struct PRMergeUseCase: Sendable {
    public init() {}

    public func allows(_ method: String, on pr: PullRequest?) -> Bool {
        guard let pr, pr.state == "open", !pr.hasConflicts, pr.mergeBlockedReason == nil else { return false }
        return pr.methods.contains(method)
    }
}
