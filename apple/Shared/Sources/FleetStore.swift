import Foundation
import Observation

@MainActor
@Observable
public final class FleetStore {
    public private(set) var overview: Overview?
    public private(set) var connected = false
    public var lastError: String?

    private let client: AlondraClient
    private var syncTask: Task<Void, Never>?

    public init(client: AlondraClient = .local) {
        self.client = client
    }

    public var questions: [Question] { overview?.openQuestions ?? [] }
    public var news: [Report] { overview?.news ?? [] }
    public var sentToOrchestrator: [SentMessage] { (overview?.orchestratorMessages ?? []).filter { $0.from == "david" } }
    public var nodes: [Node] { overview?.nodes ?? [] }

    public var inbox: [InboxItem] {
        let questions = questions.map(InboxItem.question).sorted { $0.date > $1.date }
        let reports = news.map(InboxItem.report).sorted { $0.date > $1.date }
        return questions + reports
    }

    public func items(for session: String) -> [InboxItem] {
        inbox.filter { $0.sessionId == session }
    }

    public var projects: [(name: String, nodes: [Node])] {
        Dictionary(grouping: nodes) { $0.project ?? "otros" }
            .map { (name: $0.key, nodes: $0.value) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    public func moreContext(_ question: Question, note: String) async {
        do {
            try await client.moreContext(question.id, note: note)
        } catch {
            lastError = error.localizedDescription
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
    public var orchestrator: Node? { nodes.first { $0.isOrchestrator && $0.alive } }

    public func start() {
        guard syncTask == nil else { return }
        syncTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.syncOnce()
            }
        }
    }

    public func stop() {
        syncTask?.cancel()
        syncTask = nil
    }

    private func syncOnce() async {
        do {
            let fresh = try await client.overview()
            overview = fresh
            connected = true
            try await client.waitForChange(after: fresh.tick)
        } catch is CancellationError {
            return
        } catch {
            connected = false
            try? await Task.sleep(for: .seconds(2))
        }
    }

    public func answer(_ question: Question, text: String, option: String? = nil) async {
        do {
            try await client.answer(question.id, text: text, option: option)
        } catch {
            lastError = error.localizedDescription
        }
    }

    public func send(to node: Node, text: String, acknowledge: Bool = false) async {
        do {
            try await client.send(to: node.sessionId, text: text)
            if acknowledge { try await client.acknowledge(node.sessionId) }
        } catch {
            lastError = error.localizedDescription
        }
    }

    public func reply(to report: Report, text: String) async {
        do {
            try await client.send(to: report.sessionId, text: text)
            try await client.markRead(report.id)
        } catch {
            lastError = error.localizedDescription
        }
    }

    public func markRead(_ report: Report) async {
        do {
            try await client.markRead(report.id)
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
