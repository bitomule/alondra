import AlondraCommands
import Foundation

/// Temporary presentation adapter until each TCA feature owns its own localized error copy.
extension CommandKind {
    var userFacingDescription: String {
        switch self {
        case .answer(let id): String(localized: "Answer question #\(id)", table: "AlondraShared", bundle: #bundle)
        case .moreContext(let id): String(localized: "Ask for context on question #\(id)", table: "AlondraShared", bundle: #bundle)
        case .markRead(let id): String(localized: "Mark report #\(id) read", table: "AlondraShared", bundle: #bundle)
        case .review(let id): String(localized: "Review report #\(id)", table: "AlondraShared", bundle: #bundle)
        case .send(let session): String(localized: "Message node \(session)", table: "AlondraShared", bundle: #bundle)
        case .replyToReport(let id): String(localized: "Reply to report #\(id)", table: "AlondraShared", bundle: #bundle)
        case .newRequest: String(localized: "New request", table: "AlondraShared", bundle: #bundle)
        case .closeNode(let session): String(localized: "Close node \(session)", table: "AlondraShared", bundle: #bundle)
        case .cancelTask(let id): String(localized: "Cancel task \(id)", table: "AlondraShared", bundle: #bundle)
        case .reopenTask(let id): String(localized: "Reopen task \(id)", table: "AlondraShared", bundle: #bundle)
        case .withdrawRequest(let id): String(localized: "Withdraw request \(id)", table: "AlondraShared", bundle: #bundle)
        case .archiveCompleted: String(localized: "Archive completed tasks", table: "AlondraShared", bundle: #bundle)
        case .mergePullRequest(let id): String(localized: "Merge PR #\(id)", table: "AlondraShared", bundle: #bundle)
        case .resolveConflicts(let id): String(localized: "Resolve conflicts on PR #\(id)", table: "AlondraShared", bundle: #bundle)
        case .requestChanges(let id): String(localized: "Request changes on PR #\(id)", table: "AlondraShared", bundle: #bundle)
        case .askOnPullRequest(let id): String(localized: "Ask about PR #\(id)", table: "AlondraShared", bundle: #bundle)
        case .dismissPullRequest(let id): String(localized: "Set aside PR #\(id)", table: "AlondraShared", bundle: #bundle)
        case .task(let id): String(localized: "Task \(id)", table: "AlondraShared", bundle: #bundle)
        }
    }
}
