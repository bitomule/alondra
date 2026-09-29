import AlondraContracts
import Foundation

extension PullRequest {
    public var stackedOn: String? {
        belowNumber.map { String(localized: "On top of PR #\(String($0))", table: "AlondraShared", bundle: #bundle) }
    }
    public var mergeBlockedReason: String? {
        guard let below = belowNumber.map(String.init) else { return nil }
        if belowSetAside == true {
            return String(localized: "PR #\(below) is still open on GitHub: merge or close it first", table: "AlondraShared", bundle: #bundle)
        }
        return String(localized: "Merge PR #\(below) first", table: "AlondraShared", bundle: #bundle)
    }
    public var summary: String {
        var parts = ["\(repo) #\(number)"]
        if let base, let head { parts.append("\(base) ← \(head)") }
        if let additions, let deletions, let changedFiles, changedFiles > 0 {
            parts.append(String(localized: "+\(additions) −\(deletions) in \(changedFiles) files", table: "AlondraShared", bundle: #bundle))
        }
        return parts.joined(separator: " · ")
    }
    public static func methodLabel(_ method: String) -> String {
        switch method {
        case "merge": String(localized: "Merge commit", table: "AlondraShared", bundle: #bundle)
        case "squash": String(localized: "Squash", table: "AlondraShared", bundle: #bundle)
        case "rebase": String(localized: "Rebase", table: "AlondraShared", bundle: #bundle)
        default: method
        }
    }
    public enum MainAction: Equatable, Sendable {
        case resolveConflicts
        case merge(methods: [String], blockedBy: String?)
    }
    public var mainAction: MainAction {
        hasConflicts ? .resolveConflicts : .merge(methods: methods, blockedBy: mergeBlockedReason)
    }
}

extension DomainPullRequestCard {
    public enum MainAction: Equatable, Sendable {
        case resolveConflicts
        case merge(methods: [DomainV2.MergeMethod], blockedBy: String?)
    }

    public var mainAction: MainAction {
        hasConflicts ? .resolveConflicts : .merge(methods: pullRequest.allowedMethods, blockedBy: observation?.mergeBlockedReason)
    }

    public var summary: String {
        observation?.summary ?? "\(pullRequest.repository) #\(pullRequest.number)"
    }

    public var webURL: URL? {
        observation.flatMap { URL(string: $0.url) }
            ?? URL(string: "https://\(pullRequest.host)/\(pullRequest.repository)/pull/\(pullRequest.number)")
    }
}

extension DomainV2.MergeMethod {
    public var label: String { PullRequest.methodLabel(rawValue) }
}
