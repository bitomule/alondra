import Foundation

public struct PRCheck: Codable, Hashable, Sendable {
    public let name: String
    public let state: String
    public let url: String?

    public var passed: Bool { state == "passed" }
    public var failed: Bool { state == "failed" }
    public var running: Bool { state == "running" }
}

/// What GitHub reports about a registered pull request: title, checks, mergeability and stack.
/// Served as the runtime observation of a domain PR; it never decides merge or completion.
public struct PullRequest: Codable, Identifiable, Hashable, Sendable {
    public let id: UInt64
    public let url: String
    public let repo: String
    public let number: UInt64
    public let sessionId: String
    public let nodeName: String?
    public let title: String
    public let body: String?
    public let base: String?
    public let head: String?
    public let state: String
    public let checks: [PRCheck]?
    public let mergeMethods: [String]?
    public let additions: UInt64?
    public let deletions: UInt64?
    public let changedFiles: UInt64?
    public let mergeable: String?
    public let mergeState: String?
    public let openedAt: Int64
    public let preferredMethod: String?
    public let belowNumber: UInt64?
    public let belowSetAside: Bool?
    public let stackBottom: UInt64?
    public let stackOpenedAt: Int64?
    public let stackPosition: Int?
    /// The task this PR belongs to.
    public let task: String?

    public static let allMethods = ["merge", "squash", "rebase"]

    /// The merge methods the repo allows, the one last used there first.
    public var methods: [String] {
        let allowed = (mergeMethods ?? []).isEmpty ? Self.allMethods : mergeMethods ?? []
        guard let preferred = preferredMethod, allowed.contains(preferred) else { return allowed }
        return [preferred] + allowed.filter { $0 != preferred }
    }

    public var hasConflicts: Bool { mergeable == "CONFLICTING" || mergeState == "DIRTY" }

    /// GitHub answers `UNKNOWN` right after the base moves, until Alondra asks again.
    public var mergeabilityUnknown: Bool { !hasConflicts && (mergeable == nil || mergeable == "UNKNOWN") }

    public var trainOpenedAt: Int64 { stackOpenedAt ?? openedAt }
    public var trainBottom: UInt64 { stackBottom ?? id }
    public var trainPosition: Int { stackPosition ?? 0 }

    /// Each train together, bottom PR first; the newest train first.
    public static func newestTrainsFirst(_ lhs: PullRequest, _ rhs: PullRequest) -> Bool {
        if lhs.trainOpenedAt != rhs.trainOpenedAt { return lhs.trainOpenedAt > rhs.trainOpenedAt }
        if lhs.trainBottom != rhs.trainBottom { return lhs.trainBottom > rhs.trainBottom }
        return (lhs.trainPosition, lhs.id) < (rhs.trainPosition, rhs.id)
    }

}
