import Foundation

/// What the user asked for, in their words. Its state comes from its tasks.
public struct WorkRequest: Codable, Identifiable, Hashable, Sendable {
    public enum State: String, Codable, Hashable, Sendable {
        case defining
        case inProgress = "in_progress"
        case done

        public init(from decoder: Decoder) throws {
            self = State(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .inProgress
        }
    }

    public struct Options: Codable, Hashable, Sendable {
        public let project: String?
        public let autoMerge: Bool?
        public let effort: String?
        public let issue: String?
    }

    /// How a request that needed no task ended: the orchestrator answered it, or the user
    /// withdrew it.
    public struct Closure: Codable, Hashable, Sendable {
        public let answer: String?
        public let withdrawn: Bool?
        public let at: Int64
    }

    /// A request still defining this long after it was sent was left without tasks: the
    /// orchestrator defines or closes one within minutes.
    public static let leftDefiningAfter: Int64 = 10 * 60 * 1000

    public let id: String
    public let text: String?
    public let attachments: [Attachment]?
    public let options: Options?
    public let createdAt: Int64
    public let synthetic: Bool?
    public let state: State
    public let tasks: [String]?
    /// The title the orchestrator gave it.
    public let title: String?
    /// When it left the Tasks list: every task of it is archived, or it finished without tasks
    /// and was archived itself.
    public let archivedAt: Int64?
    public let closed: Closure?

    /// The orchestrator has not defined it: the user can withdraw it, and it can then be archived.
    public var canWithdraw: Bool { state == .defining }

    /// The orchestrator neither defined it nor closed it.
    public func isLeftDefining(now: Date = .now) -> Bool {
        state == .defining && Int64(now.timeIntervalSince1970 * 1000) - createdAt >= Self.leftDefiningAfter
    }

}

/// One piece of work a request became, with its own state machine on the server.
public struct WorkTask: Codable, Identifiable, Hashable, Sendable {
    public enum State: String, Codable, Hashable, Sendable, CaseIterable {
        case waitingDependency = "waiting_dependency"
        case running
        case needsYou = "needs_you"
        case inReview = "in_review"
        case closing
        case done
        case cancelled

        public var isFinished: Bool { self == .done || self == .cancelled }

        /// A state this build does not know yet reads as `running`, so the board keeps decoding
        /// when the daemon learns a new one.
        public init(from decoder: Decoder) throws {
            self = State(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .running
        }
    }

    public struct Options: Codable, Hashable, Sendable {
        public let autoMerge: Bool?
        public let effort: String?
        public let project: String?
        public let issue: String?

        public var mergesAlone: Bool { autoMerge == true }
    }

    public struct Assignee: Codable, Hashable, Sendable {
        public let sessionId: String
        public let agentId: String?
    }

    public struct Blocker: Codable, Hashable, Sendable {
        public let kind: String
        public let question: UInt64
        public let since: Int64

        public var isAction: Bool { kind == "action" }
    }

    public struct Event: Codable, Hashable, Sendable {
        public let kind: String
        public let task: String?
        public let pr: UInt64?
        public let report: UInt64?
        public let question: UInt64?
        public let blocker: Blocker?
    }

    public struct Transition: Codable, Hashable, Sendable {
        public let from: State
        public let to: State
        public let event: Event
        public let at: Int64
    }

    public let id: String
    public let request: String
    public let title: String
    public let brief: String?
    public let dependsOn: [String]
    public let options: Options?
    public let assignee: Assignee?
    public let prs: [UInt64]
    public let documents: [UInt64]
    public let blocker: Blocker?
    public let notices: [UInt64]
    public let state: State
    public let stateSince: Int64
    public let createdAt: Int64
    public let finalReport: UInt64?
    /// The PR card of a dependency whose merge the task, already started, waits for.
    public let awaiting: UInt64?
    public let history: [Transition]?
    /// Alondra merges its PR by itself once the checks pass.
    public let autoMergePending: Bool?
    /// When the user archived it, once it was finished.
    public let archivedAt: Int64?
    /// When anything last happened to it, its history included.
    public let lastChange: Int64?
    /// Made by the daemon's task log; a node's own task from before tasks were recorded is not.
    public let recorded: Bool

    private enum CodingKeys: String, CodingKey {
        case id, request, title, brief, dependsOn, options, assignee, prs, documents, blocker, notices, state, stateSince, createdAt, finalReport, awaiting,
            history, autoMergePending, archivedAt, lastChange, recorded
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        request = try c.decode(String.self, forKey: .request)
        title = try c.decode(String.self, forKey: .title)
        brief = try c.decodeIfPresent(String.self, forKey: .brief)
        dependsOn = try c.decodeIfPresent([String].self, forKey: .dependsOn) ?? []
        options = try c.decodeIfPresent(Options.self, forKey: .options)
        assignee = try c.decodeIfPresent(Assignee.self, forKey: .assignee)
        prs = try c.decodeIfPresent([UInt64].self, forKey: .prs) ?? []
        documents = try c.decodeIfPresent([UInt64].self, forKey: .documents) ?? []
        blocker = try c.decodeIfPresent(Blocker.self, forKey: .blocker)
        notices = try c.decodeIfPresent([UInt64].self, forKey: .notices) ?? []
        state = try c.decode(State.self, forKey: .state)
        stateSince = try c.decode(Int64.self, forKey: .stateSince)
        createdAt = try c.decodeIfPresent(Int64.self, forKey: .createdAt) ?? stateSince
        finalReport = try c.decodeIfPresent(UInt64.self, forKey: .finalReport)
        awaiting = try c.decodeIfPresent(UInt64.self, forKey: .awaiting)
        history = try c.decodeIfPresent([Transition].self, forKey: .history)
        autoMergePending = try c.decodeIfPresent(Bool.self, forKey: .autoMergePending)
        archivedAt = try c.decodeIfPresent(Int64.self, forKey: .archivedAt)
        lastChange = try c.decodeIfPresent(Int64.self, forKey: .lastChange)
        recorded = try c.decodeIfPresent(Bool.self, forKey: .recorded) ?? false
    }

    public var mergesAlone: Bool { autoMergePending == true }

    public var isArchived: Bool { archivedAt != nil }

    /// Moves when its history does, also when its state stays: an older alondrad only says when
    /// the state changed.
    public var historyKey: Int64 { lastChange ?? stateSince }

    /// Every report the task carries: documents, notices and its final report.
    public var reports: [UInt64] { documents + notices + (finalReport.map { [$0] } ?? []) }

    /// It left a PR, a document or a final report.
    public var delivered: Bool { !prs.isEmpty || !documents.isEmpty || finalReport != nil }

    /// When it last moved: its last change while it is open, when it finished once it is.
    public var changedAt: Int64 { state.isFinished ? stateSince : max(lastChange ?? stateSince, stateSince) }
}

/// What a node or one of its subagents is doing right now.
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
public struct TaskInFull: Decodable, Sendable {
    public let task: WorkTask
    public let documents: [Report]?

    private enum CodingKeys: String, CodingKey {
        case task
        case documents = "reports"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        task = try c.decodeIfPresent(WorkTask.self, forKey: .task) ?? WorkTask(from: decoder)
        documents = try c.decodeIfPresent([Report].self, forKey: .documents)
    }
}

/// What `GET /v1/nodes/{key}/activity` says each thread of the node does now, or the node inside
/// `GET /v1/nodes/{key}` from an alondrad without that route; the overview leaves it out.
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
