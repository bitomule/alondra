import AlondraContracts
import Foundation

extension PullRequest {
    /// "PR #150 · waiting for checks, 2 of 3", for a task's line.
    public func progress(mergesAlone: Bool) -> String {
        let number = String(number)
        let checks = checks ?? []
        if hasConflicts {
            return String(localized: "PR #\(number) · conflicts", table: "AlondraShared", bundle: #bundle)
        }
        if checks.contains(where: \.failed) {
            return String(localized: "PR #\(number) · checks failed", table: "AlondraShared", bundle: #bundle)
        }
        if let below = belowNumber {
            return String(localized: "PR #\(number) · merge #\(String(below)) first", table: "AlondraShared", bundle: #bundle)
        }
        if checks.contains(where: \.running) {
            let passed = checks.count(where: \.passed)
            return String(localized: "PR #\(number) · waiting for checks, \(passed) of \(checks.count)", table: "AlondraShared", bundle: #bundle)
        }
        if mergesAlone {
            return String(localized: "PR #\(number) · Alondra merges it when the checks pass", table: "AlondraShared", bundle: #bundle)
        }
        return String(localized: "PR #\(number) · ready to merge", table: "AlondraShared", bundle: #bundle)
    }
}

extension DomainV2.TaskTransition {
    /// What moved the task, in a few words.
    public var summary: String {
        switch cause {
        case "dependency_ready": String(localized: "a task it builds on has its PR open", table: "AlondraShared", bundle: #bundle)
        case "blocker_raised":
            switch blockerKind {
            case "permission": String(localized: "the node needs a permission", table: "AlondraShared", bundle: #bundle)
            case "action": String(localized: "the node asked you to do something", table: "AlondraShared", bundle: #bundle)
            default: String(localized: "the node asked you something", table: "AlondraShared", bundle: #bundle)
            }
        case "pr_registered": String(localized: "the node opened a PR", table: "AlondraShared", bundle: #bundle)
        case "result_presented": String(localized: "the node presented a result", table: "AlondraShared", bundle: #bundle)
        case "delivery_accepted": String(localized: "you accepted the result", table: "AlondraShared", bundle: #bundle)
        case "merge_authorized": String(localized: "you authorized the merge", table: "AlondraShared", bundle: #bundle)
        case "merged": String(localized: "merged", table: "AlondraShared", bundle: #bundle)
        case "answered": String(localized: "you answered", table: "AlondraShared", bundle: #bundle)
        case "feedback": String(localized: "you asked for changes", table: "AlondraShared", bundle: #bundle)
        case "completed": String(localized: "the node declared it finished", table: "AlondraShared", bundle: #bundle)
        case "reopen": String(localized: "reopened", table: "AlondraShared", bundle: #bundle)
        case "cancel": String(localized: "cancelled", table: "AlondraShared", bundle: #bundle)
        default: cause
        }
    }
}
