import AlondraShared
import Foundation
import Observation
import SwiftUI

@MainActor
@Observable
final class Fleet {
    enum Connection: Equatable {
        case connecting
        case online
        case offline(String)
    }

    private(set) var overview: Overview?
    private(set) var connection = Connection.connecting
    private(set) var resolvedCount = 0
    private(set) var skippedCount = 0
    var notice: Notice?
    var handoff: Handoff?
    var leaving: String?

    struct Handoff {
        let id = UUID()
        let itemID: String
        let label: String
        let action: @Sendable (FleetClient) async throws -> Void
    }

    private(set) var client: FleetClient
    private var hidden: Set<String> = []
    private var skipped: [String] = []
    private(set) var focusRequest = 0

    init(client: FleetClient) {
        self.client = client
    }

    func use(_ client: FleetClient) {
        guard client.base != self.client.base else { return }
        self.client = client
        overview = nil
        hidden = []
        skipped = []
        connection = .connecting
    }

    var nodes: [Node] { overview?.nodes ?? [] }
    var machine: Machine? { overview?.machine }
    var orchestrator: Node? { nodes.filter(\.isOrchestrator).max { !$0.alive && $1.alive } }
    var sentToOrchestrator: [SentMessage] {
        (overview?.orchestratorMessages ?? []).filter { $0.from == "david" }.sorted { $0.createdAt > $1.createdAt }
    }

    var queue: [InboxItem] {
        guard let overview else { return [] }
        let questions = overview.openQuestions.sorted { $0.askedAt < $1.askedAt }.map(InboxItem.question)
        let reports = overview.news.sorted { $0.createdAt < $1.createdAt }.map(InboxItem.report)
        let open = (questions + reports).filter { !hidden.contains($0.id) }
        let fresh = open.filter { !skipped.contains($0.id) }
        let later = skipped.compactMap { id in open.first { $0.id == id } }
        return fresh + later
    }

    func pending(for session: String) -> [InboxItem] {
        queue.filter { $0.sessionId == session }
    }

    func node(_ session: String) -> Node? {
        nodes.first { $0.sessionId == session }
    }

    var projects: [(name: String, nodes: [Node])] {
        Dictionary(grouping: nodes) { $0.project ?? "Sin proyecto" }
            .map { (name: $0.key, nodes: $0.value.sorted(by: Self.nodeOrder)) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    private static func nodeOrder(_ a: Node, _ b: Node) -> Bool {
        if a.alive != b.alive { return a.alive }
        return a.stateSince > b.stateSince
    }

    func sync() async {
        while !Task.isCancelled {
            do {
                let fresh = try await client.overview()
                apply(fresh)
                try await client.waitForChange(after: fresh.tick)
            } catch where error.isCancellation {
                return
            } catch let error as URLError where error.code == .timedOut && connection == .online {
                continue
            } catch {
                connection = .offline(error.friendlyDescription)
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    func refresh() async {
        do {
            apply(try await client.overview())
        } catch where !error.isCancellation {
            connection = .offline(error.friendlyDescription)
        } catch {}
    }

    private func apply(_ fresh: Overview) {
        let live = Set(fresh.openQuestions.map { "q\($0.id)" } + fresh.news.map { "r\($0.id)" })
        withAnimation(.smooth(duration: 0.45)) {
            overview = fresh
            hidden.formIntersection(live)
            skipped.removeAll { !live.contains($0) }
            connection = .online
        }
    }

    func focus(_ item: InboxItem) {
        skipped.removeAll { $0 == item.id }
        let ahead = queue.prefix { $0.id != item.id }.map(\.id)
        skipped.append(contentsOf: ahead)
        focusRequest += 1
    }

    func skip(_ item: InboxItem) {
        withAnimation(.spring(duration: 0.5, bounce: 0.25)) {
            skipped.removeAll { $0 == item.id }
            skipped.append(item.id)
        }
        skippedCount += 1
    }

    @discardableResult
    func resolve(_ item: InboxItem, _ action: @escaping @Sendable (FleetClient) async throws -> Void) async -> Bool {
        let client = client
        withAnimation(.spring(duration: 0.55, bounce: 0.2)) {
            _ = hidden.insert(item.id)
        }
        resolvedCount += 1
        do {
            try await action(client)
            return true
        } catch {
            withAnimation(.spring(duration: 0.5, bounce: 0.2)) {
                _ = hidden.remove(item.id)
            }
            notice = Notice("No se pudo enviar: \(error.friendlyDescription)", failure: true)
            return false
        }
    }

    func perform(_ action: @escaping @Sendable (FleetClient) async throws -> Void) async -> Bool {
        do {
            try await action(client)
            return true
        } catch {
            notice = Notice("No se pudo enviar: \(error.friendlyDescription)", failure: true)
            return false
        }
    }
}

struct Notice: Equatable {
    let id = UUID()
    let text: String
    let failure: Bool

    init(_ text: String, failure: Bool = false) {
        self.text = text
        self.failure = failure
    }
}

extension InboxItem {
    var nodeName: String? {
        switch self {
        case .question(let q): q.nodeName
        case .report(let r): r.nodeName
        }
    }

    var title: String {
        switch self {
        case .question(let q): q.title
        case .report(let r): r.title
        }
    }
}
