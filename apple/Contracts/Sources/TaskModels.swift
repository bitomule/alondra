import Foundation

public struct NodeActivity: Codable, Hashable, Sendable {
    public let agentId: String?
    public let agentType: String?
    public let task: String?
    public let tool: String?
    public let summary: String?
    public let since: Int64

    public init(agentId: String?, agentType: String?, task: String?, tool: String?, summary: String?, since: Int64) {
        self.agentId = agentId; self.agentType = agentType; self.task = task
        self.tool = tool; self.summary = summary; self.since = since
    }

    public var isMainThread: Bool { agentId == nil }

    /// "Bash bazel build //app", or whichever half is known.
    public var doing: String? {
        let text = [tool, summary].compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }.joined(separator: " ")
        return text.isEmpty ? nil : text
    }
}

/// A task as its own route gives it: with its history and, from a newer alondrad, its documents,
/// which the overview only has among the newest reports.
public struct NodeThreads: Decodable, Sendable {
    private struct Detail: Decodable, Sendable {
        let activity: [NodeActivity]?
    }

    private enum CodingKeys: String, CodingKey {
        case activity, node
    }

    /// Nil from an alondrad without the activity ledger.
    public let activity: [NodeActivity]?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        activity = try c.decodeIfPresent([NodeActivity].self, forKey: .activity) ?? c.decodeIfPresent(Detail.self, forKey: .node)?.activity
    }
}

/// Whether a node works on something right now; its tasks carry everything else.
