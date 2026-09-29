import Foundation

/// `GET /runtime`: what alondrad knows about the Mac and its Claude sessions beyond the
/// domain, keyed by domain IDs. Display only; it never decides an action. Every entry is read
/// on its own, so one this build cannot decode costs only that entry.
extension DomainV2 {
    public struct Runtime: Equatable, Sendable {
        public let machine: Machine?
        public let usage: Usage?
        public let nodes: [Node]
        public let tasks: [String: TaskRuntime]
        public let requests: [String: RequestRuntime]
        public let pullRequests: [String: PullRequestRuntime]
        /// When each inbox entry arrived, by inbox ID.
        public let inboxTimes: [String: Int64]
        /// The orchestrator's executions: phase, the run waiting for a decision, queue depth.
        public let orchestrator: OrchestratorRuntime?

        public init(machine: Machine? = nil, usage: Usage? = nil, nodes: [Node] = [], tasks: [String: TaskRuntime] = [:],
                    requests: [String: RequestRuntime] = [:], pullRequests: [String: PullRequestRuntime] = [:],
                    inboxTimes: [String: Int64] = [:], orchestrator: OrchestratorRuntime? = nil) {
            self.orchestrator = orchestrator
            self.machine = machine
            self.usage = usage
            self.nodes = nodes
            self.tasks = tasks
            self.requests = requests
            self.pullRequests = pullRequests
            self.inboxTimes = inboxTimes
        }

        /// Reads the runtime body. Its maps are keyed by domain IDs, which a snake-case key
        /// strategy would rewrite, so only the machine, usage and node entries use it.
        public static func decode(from data: Data) throws -> Runtime {
            guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "runtime is not an object"))
            }
            let snake = JSONDecoder()
            snake.keyDecodingStrategy = .convertFromSnakeCase
            func one<Value: Decodable>(_ key: String, _ type: Value.Type) -> Value? {
                guard let value = object[key], !(value is NSNull),
                      let bytes = try? JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed]) else { return nil }
                return try? snake.decode(Value.self, from: bytes)
            }
            func map<Value: Decodable>(_ key: String) -> [String: Value] {
                guard let entries = object[key] as? [String: Any] else { return [:] }
                return entries.compactMapValues { value in
                    guard let bytes = try? JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed]) else { return nil }
                    return try? JSONDecoder().decode(Value.self, from: bytes)
                }
            }
            let status = object["orchestrator"].flatMap { $0 is NSNull ? nil : $0 }
            let nodes = (object["nodes"] as? [Any] ?? []).compactMap { value -> Node? in
                var value = value
                if let status, var node = value as? [String: Any], node["is_orchestrator"] as? Bool == true,
                   node["orchestrator_runtime"] == nil || node["orchestrator_runtime"] is NSNull {
                    node["orchestrator_runtime"] = status
                    value = node
                }
                guard let bytes = try? JSONSerialization.data(withJSONObject: value) else { return nil }
                return try? snake.decode(Node.self, from: bytes)
            }
            return Runtime(machine: one("machine", Machine.self), usage: one("usage", Usage.self), nodes: nodes,
                           tasks: map("tasks"), requests: map("requests"), pullRequests: map("pull_requests"),
                           inboxTimes: map("inbox_times"), orchestrator: one("orchestrator", OrchestratorRuntime.self))
        }

        public func request(_ id: RequestId?) -> RequestRuntime? { id.flatMap { requests[$0.rawValue] } }
        public func task(_ id: TaskId?) -> TaskRuntime? { id.flatMap { tasks[$0.rawValue] } }
        public func pullRequest(_ id: PrId?) -> PullRequestRuntime? { id.flatMap { pullRequests[$0.rawValue] } }
        public func inboxTime(_ item: String) -> Int64? { inboxTimes[item] }

        public func node(_ agent: AgentId?) -> Node? {
            agent.flatMap { id in nodes.first { $0.sessionId == id.rawValue } }
        }
    }

    public struct TaskRuntime: Decodable, Equatable, Sendable {
        public let project: String?
        public let stateSince: Int64?
        public let effort: String?
        public let cancelReason: String?

        enum CodingKeys: String, CodingKey { case project, stateSince = "state_since", effort, cancelReason = "cancel_reason" }

        public init(project: String? = nil, stateSince: Int64? = nil, effort: String? = nil, cancelReason: String? = nil) {
            self.project = project
            self.stateSince = stateSince
            self.effort = effort
            self.cancelReason = cancelReason
        }
    }

    /// When a request came in and how it closed, which the domain's `closed` flag does not say.
    public struct RequestRuntime: Decodable, Equatable, Sendable {
        public let createdAt: Int64?
        public let withdrawn: Bool
        /// The orchestrator's answer when it closed the request without Tasks.
        public let answer: String?

        public init(createdAt: Int64? = nil, withdrawn: Bool = false, answer: String? = nil) {
            self.createdAt = createdAt
            self.withdrawn = withdrawn
            self.answer = answer
        }

        enum CodingKeys: String, CodingKey { case createdAt = "created_at", withdrawn, answer }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            createdAt = try container.decodeIfPresent(Int64.self, forKey: .createdAt)
            withdrawn = try container.decodeIfPresent(Bool.self, forKey: .withdrawn) ?? false
            answer = try container.decodeIfPresent(String.self, forKey: .answer)
        }
    }

    /// What GitHub last reported for a registered PR. It never decides merge or completion.
    public struct PullRequestRuntime: Decodable, Equatable, Sendable {
        public let title: String?
        public let body: String?
        public let checks: [PRCheck]
        public let conflicts: Bool
        public let mergeabilityUnknown: Bool

        public init(title: String? = nil, body: String? = nil, checks: [PRCheck] = [], conflicts: Bool = false, mergeabilityUnknown: Bool = false) {
            self.title = title
            self.body = body
            self.checks = checks
            self.conflicts = conflicts
            self.mergeabilityUnknown = mergeabilityUnknown
        }

        enum CodingKeys: String, CodingKey { case title, body, checks, conflicts, mergeabilityUnknown = "mergeability_unknown" }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            title = try container.decodeIfPresent(String.self, forKey: .title)
            body = try container.decodeIfPresent(String.self, forKey: .body)
            checks = (try? container.decodeIfPresent([Lenient<PRCheck>].self, forKey: .checks))?.compactMap(\.value) ?? []
            conflicts = try container.decodeIfPresent(Bool.self, forKey: .conflicts) ?? false
            mergeabilityUnknown = try container.decodeIfPresent(Bool.self, forKey: .mergeabilityUnknown) ?? false
        }

        public var hasConflicts: Bool { conflicts }
    }

    /// `GET /tasks/{id}/history`: the brief the Task was launched with and each state it went through.
    public struct TaskHistory: Decodable, Equatable, Sendable {
        public let brief: String?
        public let history: [TaskStep]

        public init(brief: String? = nil, history: [TaskStep] = []) {
            self.brief = brief
            self.history = history
        }

        enum CodingKeys: String, CodingKey { case brief, history }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            brief = try container.decodeIfPresent(String.self, forKey: .brief)
            history = (try? container.decodeIfPresent([Lenient<TaskStep>].self, forKey: .history))?.compactMap(\.value) ?? []
        }
    }

    public struct TaskStep: Decodable, Hashable, Sendable {
        public let at: Int64
        public let state: TaskState
        public let summary: String

        public init(at: Int64, state: TaskState, summary: String) {
            self.at = at
            self.state = state
            self.summary = summary
        }

        enum CodingKeys: String, CodingKey { case at, state, summary }

        public init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            at = try container.decode(Int64.self, forKey: .at)
            state = try container.decode(TaskState.self, forKey: .state)
            summary = try container.decodeIfPresent(String.self, forKey: .summary) ?? ""
        }
    }

    /// Decodes one entry, or nothing when this build cannot read it.
    struct Lenient<Value: Decodable>: Decodable {
        let value: Value?
        init(from decoder: any Decoder) throws { value = try? Value(from: decoder) }
    }
}
