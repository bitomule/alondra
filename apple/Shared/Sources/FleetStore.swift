import Foundation
import Observation

@MainActor
@Observable
public final class FleetStore {
    public private(set) var overview: Overview? {
        didSet { rebuild() }
    }
    public private(set) var connected = false
    public var lastError: String?

    public private(set) var inbox: [InboxItem] = []
    public private(set) var projects: [(name: String, nodes: [Node])] = []
    private var pendingBySession: [String: Int] = [:]
    private var hidden: Set<String> = [] {
        didSet { rebuild() }
    }

    private let client: AlondraClient
    private var syncTask: Task<Void, Never>?
    @ObservationIgnored private var documents: [String: String] = [:]

    public init(client: AlondraClient = .local) {
        self.client = client
    }

    public var questions: [Question] { overview?.openQuestions ?? [] }
    public var news: [Report] { overview?.news ?? [] }
    public var sentToOrchestrator: [SentMessage] { (overview?.orchestratorMessages ?? []).filter { $0.from == "david" } }
    public var nodes: [Node] { overview?.nodes ?? [] }
    public var orchestrator: Node? { nodes.first { $0.isOrchestrator && $0.alive } }

    public func items(for session: String) -> [InboxItem] {
        inbox.filter { $0.sessionId == session }
    }

    public func pendingCount(for session: String) -> Int {
        pendingBySession[session, default: 0]
    }

    public func node(_ session: String) -> Node? {
        nodes.first { $0.sessionId == session }
    }

    private func rebuild() {
        let questions = questions.map(InboxItem.question).sorted { $0.date > $1.date }
        let reports = news.map(InboxItem.report).sorted { $0.date > $1.date }
        let visible = (questions + reports).filter { !hidden.contains($0.id) }
        inbox = visible
        pendingBySession = visible.reduce(into: [:]) { $0[$1.sessionId, default: 0] += 1 }
        projects = Dictionary(grouping: nodes) { $0.project ?? "otros" }
            .map { (name: $0.key, nodes: $0.value.sorted(by: Self.attentionOrder)) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    private static func attentionOrder(_ lhs: Node, _ rhs: Node) -> Bool {
        if lhs.alive != rhs.alive { return lhs.alive }
        if lhs.state.urgency != rhs.state.urgency { return lhs.state.urgency < rhs.state.urgency }
        return lhs.displayName.localizedStandardCompare(rhs.displayName) == .orderedAscending
    }

    public func start() {
        guard syncTask == nil else { return }
        syncTask = Task { [weak self] in
            await self?.run()
        }
    }

    public func stop() {
        syncTask?.cancel()
        syncTask = nil
    }

    public func run() async {
        while !Task.isCancelled {
            await syncOnce()
        }
    }

    private func syncOnce() async {
        do {
            let fresh = try await client.overview()
            let present = Set(fresh.openQuestions.map { "q\($0.id)" } + fresh.news.map { "r\($0.id)" })
            overview = fresh
            if !hidden.isSubset(of: present) { hidden.formIntersection(present) }
            connected = true
            try await client.waitForChange(after: fresh.tick)
        } catch is CancellationError {
            return
        } catch {
            if Task.isCancelled { return }
            connected = false
            try? await Task.sleep(for: .seconds(2))
        }
    }

    private func resolving(_ id: String, _ work: () async throws -> Void) async -> Bool {
        hidden.insert(id)
        do {
            try await work()
            return true
        } catch {
            hidden.remove(id)
            lastError = error.localizedDescription
            return false
        }
    }

    @discardableResult
    public func answer(_ question: Question, text: String, option: String? = nil) async -> Bool {
        await resolving("q\(question.id)") {
            try await client.answer(question.id, text: text, option: option)
        }
    }

    @discardableResult
    public func moreContext(_ question: Question, note: String) async -> Bool {
        await resolving("q\(question.id)") {
            try await client.moreContext(question.id, note: note)
        }
    }

    @discardableResult
    public func reply(to report: Report, text: String) async -> Bool {
        await resolving("r\(report.id)") {
            try await client.send(to: report.sessionId, text: text)
            try await client.markRead(report.id)
        }
    }

    @discardableResult
    public func markRead(_ report: Report) async -> Bool {
        await resolving("r\(report.id)") {
            try await client.markRead(report.id)
        }
    }

    public func newRequest(_ text: String) async -> Bool {
        do {
            try await client.newRequest(text)
            return true
        } catch {
            lastError = error.localizedDescription
            return false
        }
    }

    public func smartSearch(_ query: String) async -> SearchResult? {
        try? await client.search(query)
    }

    public func document(at path: String) async throws -> String {
        if let cached = documents[path] { return cached }
        let text = try await client.fileText(path)
        documents[path] = text
        return text
    }

    public func send(to node: Node, text: String, acknowledge: Bool = false) async {
        do {
            try await client.send(to: node.sessionId, text: text)
            if acknowledge { try await client.acknowledge(node.sessionId) }
        } catch {
            lastError = error.localizedDescription
        }
    }

    public func acknowledge(_ node: Node) async {
        do {
            try await client.acknowledge(node.sessionId)
        } catch {
            lastError = error.localizedDescription
        }
    }
}

extension NodeState {
    public var urgency: Int {
        switch self {
        case .waitingAnswer: 0
        case .unclear: 1
        case .waitingMachine: 2
        case .working: 3
        case .delivered: 4
        case .done: 5
        }
    }
}
