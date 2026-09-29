import AlondraContracts
import AlondraDomain
import Foundation

extension DomainV2.PullRequest {
    /// "a/b #12 · main", the line under a PR's title.
    public var summary: String {
        base.isEmpty ? "\(repository) #\(number)" : "\(repository) #\(number) · \(base)"
    }

    /// Shown on a PR stacked on this one.
    public var stackedOnLabel: String {
        String(localized: "On top of PR #\(String(number))", table: "AlondraShared", bundle: #bundle)
    }

    /// Why a PR stacked on this one cannot merge yet.
    public var mergeFirstLabel: String {
        String(localized: "Merge PR #\(String(number)) first", table: "AlondraShared", bundle: #bundle)
    }

    public var webURL: URL? { URL(string: "https://\(host)/\(repository)/pull/\(number)") }
}

extension DomainPullRequestCard {
    public enum MainAction: Equatable, Sendable {
        case resolveConflicts
        case merge(methods: [DomainV2.MergeMethod], blockedBy: String?)
    }

    public var mainAction: MainAction {
        hasConflicts ? .resolveConflicts : .merge(methods: pullRequest.allowedMethods, blockedBy: stackedOn?.mergeFirstLabel)
    }

    public var summary: String { pullRequest.summary }
    public var webURL: URL? { pullRequest.webURL }
}

extension DomainV2.MergeMethod {
    public var label: String {
        switch self {
        case .merge: String(localized: "Merge commit", table: "AlondraShared", bundle: #bundle)
        case .squash: String(localized: "Squash", table: "AlondraShared", bundle: #bundle)
        case .rebase: String(localized: "Rebase", table: "AlondraShared", bundle: #bundle)
        }
    }
}
