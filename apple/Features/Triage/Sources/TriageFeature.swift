import AlondraCommands
import AlondraShared
import ComposableArchitecture
import Foundation

public enum TriageMutationOutcome: Equatable, Sendable {
    case confirmed
    case rejected(String)
    case unknown
    case unknownCommand(UUID)
    case obsolete
}

public struct TriageService: Sendable {
    public var observe: @Sendable () async -> AsyncStream<TriageInboxUpdate>
    public var mutate: @Sendable (CommandKind, String, TriageIdentity) async -> TriageMutationOutcome

    public init(
        observe: @escaping @Sendable () async -> AsyncStream<TriageInboxUpdate>,
        mutate: @escaping @Sendable (CommandKind, String, TriageIdentity) async -> TriageMutationOutcome
    ) {
        self.observe = observe
        self.mutate = mutate
    }
}

extension TriageService: DependencyKey {
    /// H installs a process-owned SyncSession/CommandGate adapter at the scene boundary.
    /// An unconfigured feature neither polls nor sends a mutation.
    public static let liveValue = TriageService(
        observe: { AsyncStream { $0.finish() } },
        mutate: { _, _, _ in .obsolete }
    )
    public static let testValue = liveValue
}

extension DependencyValues {
    public var triageService: TriageService {
        get { self[TriageService.self] }
        set { self[TriageService.self] = newValue }
    }
}

@Reducer
public struct TriageFeature {
    @ObservableState
    public struct State: Equatable {
        public var identity: TriageIdentity?
        public var items: [InboxItem] = []
        public var questionsByPullRequest: [UInt64: [Question]] = [:]
        public var reportsByPullRequest: [UInt64: [Report]] = [:]
        public var hiddenIDs: Set<String> = []
        public var pendingIDs: Set<String> = []
        public var uncertainByItem: [String: UUID] = [:]
        public var cursor: InboxPlace?
        public var presentedID: String?
        public var notice: String?
        public var hasSnapshot = false
        /// Increases for every accepted overview, including one with unchanged card IDs.
        public var snapshotRevision: UInt64 = 0
        public var workingNodes = 0
        public var waitingNodes = 0
        public var totalNodes = 0
        public var skippedCount = 0
        public var resolvedCount = 0
        public var subscriptionID = UUID()

        public init() {}

        public var queue: [InboxItem] {
            InboxRing(cursor: cursor).arranged(items.filter { !hiddenIDs.contains($0.id) })
        }

        public var position: Int {
            InboxRing(cursor: cursor).frontIndex(in: items.filter { !hiddenIDs.contains($0.id) })
        }

        public var presentedItem: InboxItem? { items.first { $0.id == presentedID } }

        public var routeTargets: [String: String] {
            var targets: [String: String] = [:]
            for item in queue {
                targets[item.id] = item.id
                if case .pullRequest(let pr) = item {
                    for question in questions(forPullRequest: pr.id) { targets["q\(question.id)"] = item.id }
                    for report in reports(forPullRequest: pr.id) { targets["r\(report.id)"] = item.id }
                }
            }
            return targets
        }

        /// A push for a child of an open PR opens the one visible parent card.
        public func routeTarget(for id: String) -> InboxItem? {
            guard let targetID = routeTargets[id] else { return nil }
            return queue.first { $0.id == targetID }
        }

        public func questions(forPullRequest id: UInt64) -> [Question] { questionsByPullRequest[id] ?? [] }
        public func reports(forPullRequest id: UInt64) -> [Report] { reportsByPullRequest[id] ?? [] }

        public static func == (lhs: Self, rhs: Self) -> Bool {
            lhs.identity == rhs.identity && lhs.items == rhs.items
                && lhs.questionsByPullRequest == rhs.questionsByPullRequest
                && lhs.reportsByPullRequest == rhs.reportsByPullRequest
                && lhs.hiddenIDs == rhs.hiddenIDs
                && lhs.uncertainByItem == rhs.uncertainByItem
                && lhs.pendingIDs == rhs.pendingIDs && lhs.cursor == rhs.cursor
                && lhs.presentedID == rhs.presentedID && lhs.notice == rhs.notice
                && lhs.hasSnapshot == rhs.hasSnapshot && lhs.snapshotRevision == rhs.snapshotRevision
                && lhs.workingNodes == rhs.workingNodes
                && lhs.waitingNodes == rhs.waitingNodes && lhs.totalNodes == rhs.totalNodes
                && lhs.skippedCount == rhs.skippedCount && lhs.resolvedCount == rhs.resolvedCount
                && lhs.subscriptionID == rhs.subscriptionID
        }
    }

    public enum Action {
        case appeared
        case disappeared
        /// Root sends this synchronously before switching SyncSession or credentials.
        case identityChanged(TriageIdentity)
        case inboxUpdated(TriageInboxUpdate)
        case focus(String)
        case skip(String)
        case skipIfCurrent(String, TriageIdentity?)
        case review(String)
        case route(String, opensDetail: Bool)
        case hide(String)
        case mutationFinished(String, TriageIdentity, TriageMutationOutcome)
        case commandResolved(UUID, rejection: String?)
        case dismissReview
        case dismissNotice
    }

    @Dependency(\.triageService) var service
    private let observeInbox = ObserveInboxUseCase()

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .appeared:
                let id = state.subscriptionID
                return .run { send in
                    for await update in await service.observe() {
                        try Task.checkCancellation()
                        await send(.inboxUpdated(update))
                    }
                }
                .cancellable(id: id, cancelInFlight: true)
            case .disappeared:
                return .cancel(id: state.subscriptionID)
            case .identityChanged(let identity):
                guard state.identity != identity else { return .none }
                state.identity = identity
                resetPresentation(&state)
                return .none
            case .inboxUpdated(let update):
                if let identity = state.identity {
                    guard identity == update.identity else { return .none }
                } else {
                    state.identity = update.identity
                }
                guard let overview = update.overview else {
                    state.hasSnapshot = false
                    return .none
                }
                let inbox = observeInbox.execute(overview)
                state.items = inbox.items
                state.questionsByPullRequest = inbox.questionsByPullRequest
                state.reportsByPullRequest = inbox.reportsByPullRequest
                state.hasSnapshot = true
                state.snapshotRevision &+= 1
                state.workingNodes = inbox.workingNodes
                state.waitingNodes = inbox.waitingNodes
                state.totalNodes = inbox.totalNodes
                let liveIDs = Set(inbox.items.map(\.id))
                state.hiddenIDs.formIntersection(liveIDs)
                // A snapshot may omit a card and then reintroduce it while its command is in
                // flight. Keep the guard until the mutation completes or identity resets.
                if let presented = state.presentedID, !liveIDs.contains(presented) {
                    state.presentedID = nil
                }
                return .none
            case .focus(let id):
                guard let item = state.queue.first(where: { $0.id == id }) else { return .none }
                var ring = InboxRing(cursor: state.cursor)
                ring.focus(item)
                state.cursor = ring.cursor
                return .none
            case .skipIfCurrent(let id, let identity):
                guard state.identity == identity else { return .none }
                return skip(&state, id: id)
            case .skip(let id):
                return skip(&state, id: id)
            case .review(let id):
                guard state.queue.contains(where: { $0.id == id }) else { return .none }
                state.presentedID = id
                return .none
            case .route(let id, let opensDetail):
                guard let item = state.routeTarget(for: id) else { return .none }
                var ring = InboxRing(cursor: state.cursor)
                ring.focus(item)
                state.cursor = ring.cursor
                if opensDetail { state.presentedID = item.id }
                return .none
            case .hide(let id):
                guard let item = state.queue.first(where: { $0.id == id }),
                      let identity = state.identity, !state.pendingIDs.contains(id), state.uncertainByItem[id] == nil,
                      let kind = mutationKind(for: item) else { return .none }
                var ring = InboxRing(cursor: state.cursor)
                ring.resolve(item, in: state.items.filter { !state.hiddenIDs.contains($0.id) })
                state.cursor = ring.cursor
                state.hiddenIDs.insert(id)
                state.pendingIDs.insert(id)
                if state.presentedID == id { state.presentedID = nil }
                return .run { send in
                    let result = await service.mutate(kind, id, identity)
                    await send(.mutationFinished(id, identity, result))
                }
            case .mutationFinished(let id, let identity, let result):
                guard state.identity == identity, state.pendingIDs.remove(id) != nil else { return .none }
                switch result {
                case .confirmed:
                    // A later snapshot that omits the card clears this tombstone.
                    state.hiddenIDs.insert(id)
                    state.resolvedCount += 1
                case .rejected(let reason):
                    state.hiddenIDs.remove(id)
                    state.notice = reason
                case .unknownCommand(let commandID):
                    state.uncertainByItem[id] = commandID
                    state.hiddenIDs.remove(id)
                    state.notice = "The result is uncertain. Checking its status…"
                case .unknown:
                    state.hiddenIDs.remove(id)
                    state.notice = String(localized: "The result is uncertain. Check the server before trying again.")
                case .obsolete:
                    state.hiddenIDs.remove(id)
                }
                return .none
            case .commandResolved(let commandID, let rejection):
                let matches = state.uncertainByItem.filter { $0.value == commandID }.map(\.key)
                for id in matches {
                    state.uncertainByItem[id] = nil
                    if rejection == nil { state.hiddenIDs.insert(id) }
                }
                if !matches.isEmpty { state.notice = rejection }
                return .none
            case .dismissReview:
                state.presentedID = nil
                return .none
            case .dismissNotice:
                state.notice = nil
                return .none
            }
        }
    }

    private func skip(_ state: inout State, id: String) -> Effect<Action> {
        let queue = state.queue
        guard queue.count > 1, let item = queue.first(where: { $0.id == id }) else { return .none }
        var ring = InboxRing(cursor: state.cursor)
        ring.move(from: item, by: 1, in: state.items.filter { !state.hiddenIDs.contains($0.id) })
        state.cursor = ring.cursor
        state.skippedCount += 1
        return .none
    }

    private func resetPresentation(_ state: inout State) {
        state.items = []
        state.questionsByPullRequest = [:]
        state.reportsByPullRequest = [:]
        state.hiddenIDs = []
        state.pendingIDs = []
        state.uncertainByItem = [:]
        state.cursor = nil
        state.presentedID = nil
        state.notice = nil
        state.hasSnapshot = false
        state.workingNodes = 0
        state.waitingNodes = 0
        state.totalNodes = 0
    }

    private func mutationKind(for item: InboxItem) -> CommandKind? {
        switch item {
        case .question: nil // Questions require an answer; skipping is local only.
        case .report(let report): .markRead(report.id)
        case .pullRequest(let pr): .dismissPullRequest(pr.id)
        }
    }
}
