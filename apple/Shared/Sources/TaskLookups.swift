import AlondraContracts
import Foundation

extension DomainV2.PullRequestRuntime {
    /// "PR #150 · waiting for checks, 2 of 3", for a task's line.
    public func progress(number: UInt64, stackedOn below: UInt64?, mergesAlone: Bool) -> String {
        let number = String(number)
        if hasConflicts {
            return String(localized: "PR #\(number) · conflicts", table: "AlondraShared", bundle: #bundle)
        }
        if checks.contains(where: \.failed) {
            return String(localized: "PR #\(number) · checks failed", table: "AlondraShared", bundle: #bundle)
        }
        if let below {
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
