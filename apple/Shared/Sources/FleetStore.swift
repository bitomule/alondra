import Foundation
import Observation
import SwiftUI

@MainActor
@Observable
public final class FleetStore {
    public enum Connection: Equatable, Sendable {
        case connecting
        case online
        case offline(String)
        case unpaired
    }

    public struct Handoff: Sendable {
        public let id = UUID()
        public let itemID: String
        public let label: String
        public let action: @Sendable (AlondraClient) async throws -> Void

        public init(itemID: String, label: String, action: @escaping @Sendable (AlondraClient) async throws -> Void) {
            self.itemID = itemID
            self.label = label
            self.action = action
        }
    }

    public private(set) var overview: Overview? {
        didSet { rebuild() }
    }
    public private(set) var connection = Connection.connecting
    public var lastError: String?
    public var refusal: String?
    public var notice: Notice?
    public var handoff: Handoff?
    public var leaving: String?
    public private(set) var resolvedCount = 0
    public private(set) var skippedCount = 0
    public private(set) var focusRequest = 0

    public private(set) var inbox: [InboxItem] = []
    private var grouping = InboxGrouping.empty
    public private(set) var projects: [(name: String, nodes: [Node])] = []
    private var pendingBySession: [String: Int] = [:]
    private var hidden: Set<String> = [] {
        didSet { rebuild() }
    }
    private var ring = InboxRing()

    public private(set) var client: AlondraClient
    private let animated: Bool
    private let retryDelay: Duration
    private var syncTask: Task<Void, Never>?
    @ObservationIgnored private var documents: [String: String] = [:]

    public init(client: AlondraClient = .local, animated: Bool = false, retryDelay: Duration = .seconds(2)) {
        self.client = client
        self.animated = animated
        self.retryDelay = retryDelay
    }

    public var connected: Bool { connection == .online }
    public var questions: [Question] { overview?.openQuestions ?? [] }
    public var news: [Report] { overview?.news ?? [] }
    public var nodes: [Node] { overview?.nodes ?? [] }
    public var machine: Machine? { overview?.machine }
    public var usage: Usage? { overview?.usage }
    public var userName: String { overview?.user ?? "" }
    public var orchestrator: Node? { nodes.filter(\.isOrchestrator).max { !$0.alive && $1.alive } }
    public var sentToOrchestrator: [SentMessage] {
        (overview?.orchestratorMessages ?? []).filter { $0.from == "david" }.sorted { $0.createdAt > $1.createdAt }
    }

    private var ordered: [InboxItem] {
        guard let overview else { return [] }
        let items = grouping.loose.map(InboxItem.question) + (overview.prs ?? []).map(InboxItem.pullRequest) + overview.news.map(InboxItem.report)
        return InboxRing.ordered(items.filter { !hidden.contains($0.id) })
    }

    public var queue: [InboxItem] {
        ring.arranged(ordered)
    }

    public var position: Int {
        ring.frontIndex(in: ordered)
    }

    public func pending(for session: String) -> [InboxItem] {
        queue.filter { $0.sessionId == session }
    }

    public func items(for session: String) -> [InboxItem] {
        inbox.filter { $0.sessionId == session }
    }

    /// The open questions shown inside a PR card, oldest first.
    public func questions(forPullRequest id: UInt64) -> [Question] {
        (grouping.byPullRequest[id] ?? []).filter { !hidden.contains("q\($0.id)") }.sorted { $0.askedAt < $1.askedAt }
    }

    public func questions(for item: InboxItem) -> [Question] {
        guard case .pullRequest(let pr) = item else { return [] }
        return questions(forPullRequest: pr.id)
    }

    public func pendingCount(for session: String) -> Int {
        pendingBySession[session, default: 0]
    }

    public func node(_ session: String) -> Node? {
        nodes.first { $0.sessionId == session }
    }

    private func rebuild() {
        grouping = InboxGrouping(questions: questions, pullRequests: overview?.prs ?? [])
        let questions = grouping.loose.map(InboxItem.question).sorted { $0.date > $1.date }
        let reports = news.map(InboxItem.report).sorted { $0.date > $1.date }
        let prs = (overview?.prs ?? []).map(InboxItem.pullRequest).sorted { $0.date > $1.date }
        let visible = (questions + prs + reports).filter { !hidden.contains($0.id) }
        inbox = visible
        pendingBySession = visible.reduce(into: [:]) { $0[$1.sessionId, default: 0] += 1 }
        projects = Dictionary(grouping: nodes) { $0.projectName ?? String(localized: "others", table: "AlondraShared", bundle: #bundle) }
            .map { (name: $0.key, nodes: $0.value.sorted(by: Self.attentionOrder)) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    private static func attentionOrder(_ lhs: Node, _ rhs: Node) -> Bool {
        if lhs.alive != rhs.alive { return lhs.alive }
        if lhs.state.urgency != rhs.state.urgency { return lhs.state.urgency < rhs.state.urgency }
        return lhs.displayName.localizedStandardCompare(rhs.displayName) == .orderedAscending
    }

    private func animate(_ animation: Animation, _ changes: () -> Void) {
        if animated {
            withAnimation(animation, changes)
        } else {
            changes()
        }
    }

    public func use(_ client: AlondraClient) {
        guard client.identity != self.client.identity else { return }
        let sameServer = client.base == self.client.base
        self.client = client
        guard !sameServer else { return }
        overview = nil
        hidden = []
        ring = InboxRing()
        documents = [:]
        connection = .connecting
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
            do {
                let fresh = try await client.overview()
                apply(fresh)
                try await client.waitForChange(after: fresh.tick)
            } catch where error.isCancellation {
                return
            } catch let error as URLError where error.code == .timedOut && connection == .online {
                continue
            } catch {
                if Task.isCancelled { return }
                connection = (error as? AlondraError) == .unauthorized ? .unpaired : .offline(error.friendlyDescription)
                try? await Task.sleep(for: retryDelay)
            }
        }
    }

    public func refresh() async {
        do {
            apply(try await client.overview())
        } catch AlondraError.unauthorized {
            connection = .unpaired
        } catch where !error.isCancellation {
            connection = .offline(error.friendlyDescription)
        } catch {}
    }

    private func apply(_ fresh: Overview) {
        let live = Set(fresh.openQuestions.map { "q\($0.id)" } + fresh.news.map { "r\($0.id)" })
        animate(.smooth(duration: 0.45)) {
            overview = fresh
            if !hidden.isSubset(of: live) { hidden.formIntersection(live) }
            connection = .online
        }
    }

    public func focus(_ item: InboxItem) {
        ring.focus(item)
        focusRequest += 1
    }

    public func skip(_ item: InboxItem) {
        animate(.spring(duration: 0.5, bounce: 0.25)) {
            ring.move(from: item, by: 1, in: ordered)
        }
        skippedCount += 1
    }

    public func previous() {
        guard ordered.count > 1 else { return }
        animate(.spring(duration: 0.5, bounce: 0.25)) {
            ring.move(by: -1, in: ordered)
        }
        skippedCount += 1
    }

    @discardableResult
    public func resolve(_ item: InboxItem, _ action: @escaping @Sendable (AlondraClient) async throws -> Void) async -> Bool {
        let client = client
        animate(.spring(duration: 0.55, bounce: 0.2)) {
            ring.resolve(item, in: ordered)
            _ = hidden.insert(item.id)
        }
        resolvedCount += 1
        do {
            try await action(client)
            return true
        } catch {
            animate(.spring(duration: 0.5, bounce: 0.2)) {
                _ = hidden.remove(item.id)
            }
            report(error)
            return false
        }
    }

    public func perform(_ action: @escaping @Sendable (AlondraClient) async throws -> Void) async -> Bool {
        do {
            try await action(client)
            return true
        } catch {
            report(error)
            return false
        }
    }

    private func report(_ error: any Error) {
        if let refused = error as? CloseRefused {
            refusal = refused.message
        } else {
            notice = Notice(String(localized: "Couldn’t send: \(error.friendlyDescription)", table: "AlondraShared", bundle: #bundle), failure: true)
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

    @discardableResult
    public func readAndClose(_ report: Report) async -> Bool {
        guard await close(session: report.sessionId) else { return false }
        return await markRead(report)
    }

    @discardableResult
    public func close(_ node: Node) async -> Bool {
        await close(session: node.sessionId)
    }

    private func close(session: String) async -> Bool {
        do {
            try await client.closeNode(session)
            return true
        } catch let refused as CloseRefused {
            refusal = refused.message
        } catch {
            lastError = error.localizedDescription
        }
        return false
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

public struct Notice: Equatable, Sendable {
    public let id = UUID()
    public let text: String
    public let failure: Bool

    public init(_ text: String, failure: Bool = false) {
        self.text = text
        self.failure = failure
    }
}

extension NodeState {
    public var urgency: Int {
        switch self {
        case .waitingAnswer, .yourTurn: 0
        case .unclear, .stalled: 1
        case .waitingMachine: 2
        case .working: 3
        case .delivered: 4
        case .done: 5
        }
    }
}
