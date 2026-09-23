import Foundation
import Observation

@MainActor
@Observable
final class FleetStore {
    private(set) var overview: Overview?
    private(set) var connected = false
    var lastError: String?

    private let client: AlondraClient
    private var syncTask: Task<Void, Never>?

    init(client: AlondraClient = .local) {
        self.client = client
    }

    var questions: [Question] { overview?.openQuestions ?? [] }
    var news: [Node] { overview?.news ?? [] }
    var sentToOrchestrator: [SentMessage] { (overview?.orchestratorMessages ?? []).filter { $0.from == "david" } }
    var nodes: [Node] { overview?.nodes ?? [] }
    var orchestrator: Node? { nodes.first { $0.isOrchestrator && $0.alive } }

    func start() {
        guard syncTask == nil else { return }
        syncTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.syncOnce()
            }
        }
    }

    func stop() {
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

    func answer(_ question: Question, text: String, option: String? = nil) async {
        do {
            try await client.answer(question.id, text: text, option: option)
        } catch {
            lastError = error.localizedDescription
        }
    }

    func send(to node: Node, text: String, acknowledge: Bool = false) async {
        do {
            try await client.send(to: node.sessionId, text: text)
            if acknowledge { try await client.acknowledge(node.sessionId) }
        } catch {
            lastError = error.localizedDescription
        }
    }

    func acknowledge(_ node: Node) async {
        do {
            try await client.acknowledge(node.sessionId)
        } catch {
            lastError = error.localizedDescription
        }
    }
}
