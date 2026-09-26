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

extension WorkTask.Transition {
    /// What moved the task, in a few words.
    public var summary: String {
        switch event.kind {
        case "dependency_ready": String(localized: "a task it builds on has its PR open", table: "AlondraShared", bundle: #bundle)
        case "awaits_merge": String(localized: "the node has nothing left but the merge of a PR it builds on", table: "AlondraShared", bundle: #bundle)
        case "blocker_raised":
            switch event.blocker?.kind {
            case "permission": String(localized: "the node needs a permission", table: "AlondraShared", bundle: #bundle)
            case "action": String(localized: "the node asked you to do something", table: "AlondraShared", bundle: #bundle)
            default: String(localized: "the node asked you something", table: "AlondraShared", bundle: #bundle)
            }
        case "pr_opened": String(localized: "the node opened a PR", table: "AlondraShared", bundle: #bundle)
        case "document_sent": String(localized: "the node sent a document for review", table: "AlondraShared", bundle: #bundle)
        case "auto_merged": String(localized: "Alondra merged its PR", table: "AlondraShared", bundle: #bundle)
        case "final_report": String(localized: "the node reported it finished", table: "AlondraShared", bundle: #bundle)
        case "answered": String(localized: "you answered", table: "AlondraShared", bundle: #bundle)
        case "changes_requested": String(localized: "you asked for changes", table: "AlondraShared", bundle: #bundle)
        case "node_updated": String(localized: "the node pushed changes and let you know again", table: "AlondraShared", bundle: #bundle)
        case "merged": String(localized: "merged", table: "AlondraShared", bundle: #bundle)
        case "review_closed": String(localized: "the review closed without a merge", table: "AlondraShared", bundle: #bundle)
        case "reopen": String(localized: "reopened", table: "AlondraShared", bundle: #bundle)
        case "cancel": String(localized: "cancelled", table: "AlondraShared", bundle: #bundle)
        default: event.kind
        }
    }
}

extension WorkRequest {
    /// The first line of what the user asked, shortened.
    public var headline: String? {
        let line = (text ?? "").split(whereSeparator: \.isNewline).first.map { String($0).trimmingCharacters(in: .whitespaces) } ?? ""
        guard !line.isEmpty else { return nil }
        return line.count <= 90 ? line : String(line.prefix(89)).trimmingCharacters(in: .whitespaces) + "…"
    }
}
