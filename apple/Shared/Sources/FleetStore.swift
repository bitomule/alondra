import Foundation
import Observation
import SwiftUI
import AlondraCommands

@MainActor
@Observable
public final class FleetStore {
    public typealias MutationKind = CommandKind
    public typealias MutationGate = CommandGate
    public typealias MutationScope = CommandScope
    public enum Connection: Equatable, Sendable {
        case connecting
        case online
        case offline(String)
        case unpaired
    }

    public struct Handoff: Sendable {
        public let id = UUID()
        public let presentationToken: UUID
        public let itemID: String
        public let label: String
        public let kind: MutationKind
        public let action: @Sendable (AlondraClient) async throws -> Void

        public init(presentationToken: UUID, itemID: String, label: String, kind: MutationKind, action: @escaping @Sendable (AlondraClient) async throws -> Void) {
            self.presentationToken = presentationToken
            self.itemID = itemID
            self.label = label
            self.kind = kind
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
    public private(set) var handoff: Handoff?
    public private(set) var presentationToken = UUID()
    public var leaving: String?
    public private(set) var resolvedCount = 0
    public private(set) var skippedCount = 0
    public private(set) var focusRequest = 0

    public private(set) var inbox: [InboxItem] = []
    private var grouping = InboxGrouping.empty
    public private(set) var projects: [(name: String, nodes: [Node])] = []
    public private(set) var board = TaskBoard.empty
    /// alondrad answered 404 to a task action: it does not have them yet, so the apps disable them.
    public private(set) var taskActionsUnavailable = false
    /// alondrad answered 404 to an archive: it cannot archive yet, so the apps disable it.
    public private(set) var archiveUnavailable = false
    @ObservationIgnored private var taskDetailUnavailable = false
    private var pendingBySession: [String: Int] = [:]
    private var hidden: Set<String> = [] {
        didSet { rebuild() }
    }
    private var ring = InboxRing()

    public private(set) var client: AlondraClient
    private let animated: Bool
    private let retryDelay: Duration
    private let commandGate: CommandGate
    private var credentialRevision: String
    private var credentialGeneration: UInt64 = 0
    private var commandsEnabled = true
    private var syncTask: Task<Void, Never>?
    @ObservationIgnored private var documents: [String: String] = [:]
    /// What a node's own route last said its threads do, kept for when asking it again fails.
    @ObservationIgnored var threadsShown: (session: String, threads: [NodeActivity])?

    public init(client: AlondraClient = .local, animated: Bool = false, retryDelay: Duration = .seconds(2), stuckAfter: Duration = .seconds(120), commandGate: CommandGate? = nil, commandScope: CommandScope? = nil) {
        let revision = commandScope?.credentialRevision ?? UUID().uuidString
        let generation = commandScope?.generation ?? 0
        self.client = client
        self.animated = animated
        self.retryDelay = retryDelay
        self.credentialRevision = revision
        self.credentialGeneration = generation
        self.commandGate = commandGate ?? CommandGate(scope: CommandScope(server: client.base, deviceID: client.signer?.deviceID ?? client.device, credentialRevision: revision, generation: generation))
    }

    private func commandScope(_ client: AlondraClient) -> CommandScope {
        CommandScope(server: client.base, deviceID: client.signer?.deviceID ?? client.device, credentialRevision: credentialRevision, generation: credentialGeneration)
    }

    public var connected: Bool { connection == .online }
    public var questions: [Question] { (overview?.openQuestions ?? []).filter { $0.toOrchestrator != true } }
    public var news: [Report] { (overview?.news ?? []).filter { $0.toOrchestrator != true } }
    public var nodes: [Node] { overview?.nodes ?? [] }
    public var machine: Machine? { overview?.machine }
    public var usage: Usage? { overview?.usage }
    public var userName: String { overview?.user ?? "" }
    public var orchestrator: Node? { nodes.filter(\.isOrchestrator).max { $0.stateSince < $1.stateSince } }
    public var sentToOrchestrator: [SentMessage] {
        (overview?.orchestratorMessages ?? []).filter { $0.from == "david" }.sorted { $0.createdAt > $1.createdAt }
    }

    private var ordered: [InboxItem] {
        guard let overview else { return [] }
        let items = grouping.loose.map(InboxItem.question) + (overview.prs ?? []).map(InboxItem.pullRequest) + grouping.looseReports.map(InboxItem.report)
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

    /// The unread reports shown inside a PR card, oldest first.
    public func reports(forPullRequest id: UInt64) -> [Report] {
        (grouping.reportsByPullRequest[id] ?? []).filter { !hidden.contains("r\($0.id)") }.sorted { $0.createdAt < $1.createdAt }
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
        grouping = InboxGrouping(questions: questions, reports: news, pullRequests: overview?.prs ?? [])
        board = TaskBoard(requests: overview?.requests ?? [], tasks: overview?.tasks ?? [])
        let questions = grouping.loose.map(InboxItem.question).sorted { $0.date > $1.date }
        let reports = grouping.looseReports.map(InboxItem.report).sorted { $0.date > $1.date }
        let prs = (overview?.prs ?? []).sorted(by: PullRequest.newestTrainsFirst).map(InboxItem.pullRequest)
        let visible = (questions + prs + reports).filter { !hidden.contains($0.id) }
        inbox = visible
        pendingBySession = visible.reduce(into: [:]) { $0[$1.sessionId, default: 0] += 1 }
        // The orchestrator is never one more node in a project's folder: it has its own fixed
        // area outside this stable-ordered list (see `orchestrator` above).
        projects = Dictionary(grouping: nodes.filter { !$0.isOrchestrator }) { $0.projectName ?? String(localized: "others", table: "AlondraShared", bundle: #bundle) }
            .map { (name: $0.key, nodes: $0.value.sorted(by: Self.creationOrder)) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// Oldest node first; state does not drive the order, so a state change never moves a node.
    /// The session id breaks ties deterministically.
    private static func creationOrder(_ lhs: Node, _ rhs: Node) -> Bool {
        if lhs.createdAt != rhs.createdAt { return lhs.createdAt < rhs.createdAt }
        return lhs.sessionId < rhs.sessionId
    }

    private func animate(_ animation: Animation, _ changes: () -> Void) {
        if animated {
            withAnimation(animation, changes)
        } else {
            changes()
        }
    }

    public func use(_ client: AlondraClient, commandScope nextScope: CommandScope? = nil) {
        commandsEnabled = true
        let identityChanged = client.identity != self.client.identity
        self.client = client
        if let nextScope {
            if commandScope(client) != nextScope || identityChanged {
                resetPresentationForIdentityChange(to: nextScope)
            }
        } else if identityChanged {
            resetPresentationForIdentityChange()
        }
    }

    /// An invalid saved address must not leave the previous server available for mutations.
    public func suspendForInvalidConfiguration(_ reason: String) {
        commandsEnabled = false
        resetPresentationForIdentityChange()
        connection = .offline(reason)
    }

    /// The root calls this on every credential generation change, even if URL and device ID
    /// are unchanged. It invalidates any in-flight mutation's right to update presentation.
    public func resetPresentationForIdentityChange(to nextScope: CommandScope? = nil) {
        presentationToken = UUID()
        handoff = nil
        notice = nil
        refusal = nil
        lastError = nil
        leaving = nil
        credentialRevision = nextScope?.credentialRevision ?? UUID().uuidString
        credentialGeneration = nextScope?.generation ?? (credentialGeneration &+ 1)
        uncertainSends = []
        overview = nil
        hidden = []
        ring = InboxRing()
        documents = [:]
        threadsShown = nil
        taskActionsUnavailable = false
        taskDetailUnavailable = false
        connection = .connecting
    }

    public func publishHandoff(_ next: Handoff) {
        guard next.presentationToken == presentationToken else { return }
        handoff = next
    }

    public func takeHandoff(for itemID: String) -> Handoff? {
        guard let handoff, handoff.presentationToken == presentationToken, handoff.itemID == itemID else { return nil }
        self.handoff = nil
        return handoff
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
            let revision = credentialRevision
            let pollingClient = client
            do {
                let fresh = try await pollingClient.overview()
                guard revision == credentialRevision else { continue }
                apply(fresh)
                try await pollingClient.waitForChange(after: fresh.tick)
            } catch where error.isCancellation {
                return
            } catch let error as URLError where error.code == .timedOut && connection == .online {
                continue
            } catch {
                if Task.isCancelled { return }
                guard revision == credentialRevision else { continue }
                connection = (error as? AlondraError) == .unauthorized ? .unpaired : .offline(error.friendlyDescription)
                try? await Task.sleep(for: retryDelay)
            }
        }
    }

    public func refresh() async {
        await refresh(ifCurrentClientMatches: client.identity)
    }

    /// A background notification may finish after the selected server changes.
    /// The revision also guards key rotation at the same server URL.
    public func refresh(ifCurrentClientMatches identity: String) async {
        guard client.identity == identity else { return }
        let revision = credentialRevision
        let pollingClient = client
        do {
            let fresh = try await pollingClient.overview()
            guard revision == credentialRevision, client.identity == identity else { return }
            apply(fresh)
        } catch AlondraError.unauthorized {
            guard revision == credentialRevision, client.identity == identity else { return }
            connection = .unpaired
        } catch where !error.isCancellation {
            guard revision == credentialRevision, client.identity == identity else { return }
            connection = .offline(error.friendlyDescription)
        } catch {}
    }

    /// Projection seam for the root's SyncSession; FleetStore does not own that poll loop.
    public func setConnection(_ state: Connection) {
        connection = state
    }

    public func apply(_ fresh: Overview) {
        let live = Set(fresh.openQuestions.map { "q\($0.id)" } + fresh.news.map { "r\($0.id)" } + (fresh.prs ?? []).map { "p\($0.id)" })
        let reconnected = connection != .online
        if reconnected || fresh.tick < overview?.tick ?? 0 { forgetMissingRoutes() }
        animate(.smooth(duration: 0.45)) {
            overview = fresh
            if !hidden.isSubset(of: live) { hidden.formIntersection(live) }
            connection = .online
        }
        // A reconnect only refreshes state. An unanswered mutation must never be replayed.
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
    public func resolve(_ item: InboxItem, kind: MutationKind, expectedPresentationToken: UUID? = nil, _ action: @escaping @Sendable (AlondraClient) async throws -> Void) async -> Bool {
        if let expectedPresentationToken, expectedPresentationToken != presentationToken { return false }
        let client = client
        animate(.spring(duration: 0.55, bounce: 0.2)) {
            ring.resolve(item, in: ordered)
            _ = hidden.insert(item.id)
        }
        let result = await submit(kind, itemID: item.id, using: client, action)
        switch result {
        case .confirmed:
            resolvedCount += 1
            return true
        case .unknown:
            hidden.remove(item.id)
            return false
        case .obsolete: return false
        case .rejected(let error):
            animate(.spring(duration: 0.5, bounce: 0.2)) {
                _ = hidden.remove(item.id)
            }
            report(error)
            return false
        }
    }

    public func perform(_ kind: MutationKind, _ action: @escaping @Sendable (AlondraClient) async throws -> Void) async -> Bool {
        let result = await submit(kind, itemID: nil, using: client, action)
        switch result {
        case .confirmed: return true
        case .unknown: return false
        case .obsolete: return false
        case .rejected(let error):
            report(error)
            return false
        }
    }

    public private(set) var uncertainSends: [UncertainCommand] = []
    public var uncertainActionDescription: String {
        uncertainSends.first?.kind.userFacingDescription ?? String(localized: "Action", table: "AlondraShared", bundle: #bundle)
    }

    /// Restore only warnings for the current credential. A read failure remains visible and
    /// CommandGate refuses to send until the journal can be read again.
    public func restoreUncertainSends() async {
        let scope = commandScope(client)
        await commandGate.use(scope)
        guard commandsEnabled, commandScope(client) == scope else { return }
        do {
            let restored = try await commandGate.restore()
            guard commandsEnabled, commandScope(client) == scope else { return }
            uncertainSends = restored
            for itemID in restored.compactMap(\.itemID) { hidden.remove(itemID) }
        } catch {
            guard commandsEnabled, commandScope(client) == scope else { return }
            lastError = String(localized: "Cannot read saved command history")
        }
    }

    /// Dismiss only after the journal has removed every warning. This never resends a command.
    public func dismissUncertainSends() async {
        let scope = commandScope(client)
        let dismissed = uncertainSends
        let ids = dismissed.map(\.id)
        for id in ids {
            guard commandScope(client) == scope else { return }
            await commandGate.dismiss(id, in: scope)
        }
        let remaining = await commandGate.uncertain
        guard commandScope(client) == scope else { return }
        guard ids.allSatisfy({ id in !remaining.contains(where: { $0.id == id }) }) else {
            lastError = String(localized: "Cannot update saved command history")
            return
        }
        for itemID in dismissed.compactMap(\.itemID) { hidden.remove(itemID) }
        uncertainSends.removeAll { command in ids.contains(command.id) }
    }

    private func submit(_ kind: CommandKind, itemID: String?, using client: AlondraClient, _ action: @escaping @Sendable (AlondraClient) async throws -> Void) async -> CommandResult {
        guard commandsEnabled, self.client.identity == client.identity else { return .obsolete }
        let scope = commandScope(client)
        await commandGate.use(scope)
        guard commandsEnabled, self.client.identity == client.identity, commandScope(client) == scope else { return .obsolete }
        let result = await commandGate.submit(kind, itemID: itemID, in: scope, isUnknown: { $0.isTransient || $0.isCancellation }) {
            try await action(client)
        }
        guard commandsEnabled, self.client.identity == client.identity, commandScope(client) == scope else { return .obsolete }
        if case .unknown(let command) = result,
           !uncertainSends.contains(where: { $0.id == command.id }) {
            uncertainSends.append(command)
        }
        return result
    }

    /// Headless callers share the app's scoped, durable command gate and uncertainty UI.
    public func submitCommand(_ kind: CommandKind, itemID: String?, expectedCredentialRevision: String, using client: AlondraClient,
        _ action: @escaping @Sendable (AlondraClient) async throws -> Void) async -> CommandResult {
        guard expectedCredentialRevision == credentialRevision else { return .obsolete }
        return await submit(kind, itemID: itemID, using: client, action)
    }

    public func isHidden(_ itemID: String) -> Bool {
        hidden.contains(itemID)
    }

    private func report(_ error: any Error) {
        if let refused = error as? CloseRefused {
            refusal = refused.message
        } else {
            let message = String(localized: "Couldn’t send: \(error.friendlyDescription)", table: "AlondraShared", bundle: #bundle)
            notice = Notice(message, failure: true)
            lastError = message
        }
    }

    private func resolving(_ id: String, kind: CommandKind, _ work: @escaping @Sendable (AlondraClient) async throws -> Void) async -> Bool {
        hidden.insert(id)
        switch await submit(kind, itemID: id, using: client, work) {
        case .confirmed: return true
        case .unknown:
            hidden.remove(id)
            return false
        case .obsolete: return false
        case .rejected(let error):
            hidden.remove(id)
            lastError = error.localizedDescription
            return false
        }
    }

    @discardableResult
    public func answer(_ question: Question, text: String, option: String? = nil, attachments: Attachments? = nil) async -> Bool {
        await resolving("q\(question.id)", kind: .answer(question.id)) { client in
            try await client.answer(question.id, text: text, option: option, attachments: await attachments?.storedIDs(using: client) ?? [])
        }
    }

    @discardableResult
    public func moreContext(_ question: Question, note: String, attachments: Attachments? = nil) async -> Bool {
        await resolving("q\(question.id)", kind: .moreContext(question.id)) { client in
            try await client.moreContext(question.id, note: note, attachments: await attachments?.storedIDs(using: client) ?? [])
        }
    }

    @discardableResult
    public func reply(to report: Report, text: String, attachments: Attachments? = nil) async -> Bool {
        await resolving("r\(report.id)", kind: .replyToReport(report.id)) { client in
            try await client.send(to: report.sessionId, text: text, attachments: await attachments?.storedIDs(using: client) ?? [])
            try await client.markRead(report.id)
        }
    }

    @discardableResult
    public func markRead(_ report: Report) async -> Bool {
        await resolving("r\(report.id)", kind: .markRead(report.id)) { client in
            try await client.markRead(report.id)
        }
    }

    @discardableResult
    public func review(_ report: Report, _ verdict: ReviewVerdict) async -> Bool {
        await resolving("r\(report.id)", kind: .review(report.id)) { client in
            try await client.review(report.id, verdict)
        }
    }

    @discardableResult
    public func close(_ node: Node) async -> Bool {
        await close(session: node.sessionId, force: true) == .closed
    }

    enum CloseOutcome { case closed, refused, failed }

    /// A refusal is alondrad's answer about the node, shown as `refusal`; a failure means the
    /// request itself did not get through.
    private func close(session: String, force: Bool) async -> CloseOutcome {
        let result = await submit(.closeNode(session), itemID: nil, using: client) {
            try await $0.closeNode(session, force: force)
        }
        switch result {
        case .confirmed: return .closed
        case .unknown, .obsolete: return .failed
        case .rejected(let refused as CloseRefused):
            refusal = refused.message
            return .refused
        case .rejected(let error):
            lastError = error.localizedDescription
            return .failed
        }
    }

    public func newRequest(_ body: NewRequestBody, attachments: Attachments? = nil) async -> Bool {
        await perform(.newRequest) { client in
            var body = body
            body.attachments = try await attachments?.storedIDs(using: client) ?? []
            try await client.newRequest(body)
        }
    }

    @discardableResult
    public func cancel(_ task: WorkTask) async -> Bool {
        await taskAction(.cancelTask(task.id)) { try await $0.cancelTask(task.id) }
    }

    @discardableResult
    public func reopen(_ task: WorkTask) async -> Bool {
        await taskAction(.reopenTask(task.id)) { try await $0.reopenTask(task.id) }
    }

    /// Withdraws a request the orchestrator has not defined, so it stops waiting to be and can be
    /// archived.
    @discardableResult
    public func withdraw(_ request: WorkRequest) async -> Bool {
        guard request.canWithdraw else { return false }
        return await perform(.withdrawRequest(request.id)) { try await $0.withdrawRequest(request.id) }
    }

    public var canArchive: Bool { !archiveUnavailable && board.hasArchivable }

    /// alondrad came back, maybe a newer one: the routes an older one lacked are tried again.
    private func forgetMissingRoutes() {
        taskActionsUnavailable = false
        taskDetailUnavailable = false
        archiveUnavailable = false
    }

    /// Asks the node about its PR. Asking never parks the card, so unlike `resolve` it stays
    /// where it is.
    @discardableResult
    public func ask(onPullRequest id: UInt64, text: String, attachments: Attachments? = nil) async -> Bool {
        await perform(.askOnPullRequest(id)) { [attachments] client in
            try await client.ask(onPullRequest: id, text: text, attachments: try await attachments?.storedIDs(using: client) ?? [])
        }
    }

    @discardableResult
    public func archiveCompleted() async -> Bool {
        let result = await submit(.archiveCompleted, itemID: nil, using: client) {
            try await $0.archiveCompletedTasks()
        }
        switch result {
        case .confirmed: return true
        case .unknown, .obsolete: return false
        case .rejected(let error) where (error as? AlondraError) == .unexpected(404):
            archiveUnavailable = true
            return false
        case .rejected(let error):
            report(error)
            return false
        }
    }

    private func taskAction(_ kind: MutationKind, _ action: @escaping @Sendable (AlondraClient) async throws -> Void) async -> Bool {
        let result = await submit(kind, itemID: nil, using: client, action)
        switch result {
        case .confirmed: return true
        case .unknown, .obsolete: return false
        case .rejected(let error) where (error as? AlondraError) == .unexpected(404):
            taskActionsUnavailable = true
            return false
        case .rejected(let error):
            report(error)
            return false
        }
    }

    /// The task in full, which only its own route carries; what was shown already when the route
    /// fails or this alondrad has none.
    public func details(of task: WorkTask, keeping shown: TaskInFull?) async -> TaskInFull? {
        guard !taskDetailUnavailable else { return shown }
        do {
            return try await client.task(task.id)
        } catch AlondraError.unexpected(404) {
            taskDetailUnavailable = true
            return shown
        } catch {
            return shown
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

    public func send(to node: Node, text: String, attachments: Attachments? = nil) async {
        _ = await perform(.send(node.sessionId)) { client in
            try await client.send(to: node.sessionId, text: text, attachments: try await attachments?.storedIDs(using: client) ?? [])
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
