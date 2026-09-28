import AlondraContracts
import AlondraDomain
import AlondraShared
import ComposableArchitecture
import Foundation

private enum DomainWorkspaceClientKey: DependencyKey {
    /// The scene boundary installs the process-owned client. An unconfigured feature neither
    /// observes nor sends a command.
    static let liveValue = DomainWorkspaceClient.inert
    static let testValue = liveValue
}

extension DependencyValues {
    public var domainWorkspace: DomainWorkspaceClient {
        get { self[DomainWorkspaceClientKey.self] }
        set { self[DomainWorkspaceClientKey.self] = newValue }
    }
}

public enum TriageMutationOutcome: Equatable, Sendable {
    case confirmed
    case rejected(String)
    case unknownCommand(UUID)
    case obsolete

    public init(_ result: DomainCommandResult) {
        switch result {
        case .recorded: self = .confirmed
        case .rejected(let rejection): self = .rejected(rejection.message)
        case .uncertain(let id): self = .unknownCommand(id)
        case .obsolete: self = .obsolete
        }
    }
}

/// The inbox queue: pending server roots in server order. Children stay inside their root;
/// the only local state is the cursor, cards hidden while a command is in flight and drafts.
@Reducer
public struct TriageFeature {
    @ObservableState
    public struct State: Equatable {
        public var identity: TriageIdentity?
        public var workspace: DomainWorkspace?
        public var items: [DomainV2.InboxItem] = []
        public var hiddenIDs: Set<String> = []
        public var pendingIDs: Set<String> = []
        public var deferredReadIDs: Set<String> = []
        public var uncertainByItem: [String: UUID] = [:]
        public var cursor: InboxPlace?
        public var presentedID: String?
        public var noticeIsMutationFailure = false
        public var notice: String?
        public var connection: TriageConnection = .connecting
        public var hasSnapshot = false
        /// Increases for every accepted projection, including one with unchanged card IDs.
        public var snapshotRevision: UInt64 = 0
        public var skippedCount = 0
        public var resolvedCount = 0
        public var subscriptionID = UUID()

        public init() {}

        public var queue: [DomainV2.InboxItem] {
            InboxRing(cursor: cursor).arranged(items.filter { !hiddenIDs.contains($0.id) })
        }

        public var position: Int {
            InboxRing(cursor: cursor).frontIndex(in: items.filter { !hiddenIDs.contains($0.id) })
        }

        public var presentedItem: DomainV2.InboxItem? { items.first { $0.id == presentedID } }

        public var counts: DomainV2.InboxCounts? { workspace?.counts }

        /// A push for a child opens the one visible root the server placed it in.
        public func routeTarget(for id: String) -> DomainV2.InboxItem? {
            guard let root = workspace?.root(containing: id) else { return nil }
            return queue.first { $0.id == root.id }
        }
    }

    public enum Action {
        case appeared
        case disappeared
        /// Root sends this synchronously before switching the session or credentials.
        case identityChanged(TriageIdentity)
        case workspaceUpdated(DomainWorkspaceUpdate)
        case focus(String)
        case skip(String)
        case skipIfCurrent(String, TriageIdentity?)
        case review(String)
        case route(String, opensDetail: Bool)
        case hide(String)
        case readWithPresentation(String)
        case readAcknowledged(String, confirmed: Bool)
        case readPresentationFinished(String)
        case mutationFinished(String, TriageIdentity, TriageMutationOutcome)
        case commandResolved(UUID, rejection: String?)
        case dismissReview
        case dismissNotice
    }

    @Dependency(\.domainWorkspace) var client

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .appeared:
                let id = state.subscriptionID
                return .run { send in
                    for await update in await client.observe() {
                        try Task.checkCancellation()
                        await send(.workspaceUpdated(update))
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
            case .workspaceUpdated(let update):
                if let identity = state.identity {
                    guard identity == update.identity else { return .none }
                } else {
                    state.identity = update.identity
                }
                state.connection = update.connection
                guard let workspace = update.workspace else {
                    state.hasSnapshot = false
                    return .none
                }
                state.workspace = workspace
                let incoming = workspace.roots.filter(\.pending)
                // Preserve an acknowledged card until its confirmation stamp finishes.
                let incomingIDs = Set(incoming.map(\.id))
                let retained = state.items.filter { state.deferredReadIDs.contains($0.id) && !incomingIDs.contains($0.id) }
                state.items = InboxRing.ordered(incoming + retained)
                state.hasSnapshot = true
                state.snapshotRevision &+= 1
                let liveIDs = Set(incoming.map(\.id))
                state.hiddenIDs.formIntersection(liveIDs)
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
            case .readAcknowledged: return .none
            case .readPresentationFinished(let id):
                guard state.deferredReadIDs.remove(id) != nil else { return .none }
                state.hiddenIDs.insert(id)
                state.resolvedCount += 1
                return .none
            case .readWithPresentation(let id):
                guard let item = state.queue.first(where: { $0.id == id }),
                      let intent = readIntent(for: item), let workspace = state.workspace,
                      let identity = state.identity, !state.pendingIDs.contains(id),
                      !state.deferredReadIDs.contains(id), state.uncertainByItem[id] == nil else { return .none }
                state.deferredReadIDs.insert(id)
                state.pendingIDs.insert(id)
                return .run { send in
                    let result = await client.perform(intent, workspace, [])
                    await send(.mutationFinished(id, identity, TriageMutationOutcome(result)))
                }
            case .hide(let id):
                guard let item = state.queue.first(where: { $0.id == id }),
                      let intent = readIntent(for: item), let workspace = state.workspace,
                      let identity = state.identity, !state.pendingIDs.contains(id),
                      state.uncertainByItem[id] == nil else { return .none }
                var ring = InboxRing(cursor: state.cursor)
                ring.resolve(item, in: state.items.filter { !state.hiddenIDs.contains($0.id) })
                state.cursor = ring.cursor
                state.hiddenIDs.insert(id)
                state.pendingIDs.insert(id)
                if state.presentedID == id { state.presentedID = nil }
                return .run { send in
                    let result = await client.perform(intent, workspace, [])
                    await send(.mutationFinished(id, identity, TriageMutationOutcome(result)))
                }
            case .mutationFinished(let id, let identity, let result):
                guard state.identity == identity, state.pendingIDs.remove(id) != nil else { return .none }
                switch result {
                case .confirmed:
                    if state.deferredReadIDs.contains(id) { return .send(.readAcknowledged(id, confirmed: true)) }
                    // A later projection that omits the card clears this tombstone.
                    state.hiddenIDs.insert(id)
                    state.resolvedCount += 1
                case .rejected(let reason):
                    state.hiddenIDs.remove(id)
                    state.noticeIsMutationFailure = true
                    state.notice = reason
                    if state.deferredReadIDs.remove(id) != nil { return .send(.readAcknowledged(id, confirmed: false)) }
                case .unknownCommand(let commandID):
                    state.uncertainByItem[id] = commandID
                    state.hiddenIDs.remove(id)
                    state.noticeIsMutationFailure = false
                    state.notice = String(localized: "The result is uncertain. Checking its status…")
                case .obsolete:
                    state.hiddenIDs.remove(id)
                    if state.deferredReadIDs.remove(id) != nil { return .send(.readAcknowledged(id, confirmed: false)) }
                }
                return .none
            case .commandResolved(let commandID, let rejection):
                let matches = state.uncertainByItem.filter { $0.value == commandID }.map(\.key)
                var effects: [Effect<Action>] = []
                for id in matches {
                    state.uncertainByItem[id] = nil
                    if state.deferredReadIDs.contains(id) {
                        if rejection != nil { state.deferredReadIDs.remove(id) }
                        effects.append(.send(.readAcknowledged(id, confirmed: rejection == nil)))
                    } else if rejection == nil { state.hiddenIDs.insert(id) }
                }
                if !matches.isEmpty { state.notice = rejection; state.noticeIsMutationFailure = rejection != nil }
                return .merge(effects)
            case .dismissReview:
                state.presentedID = nil
                return .none
            case .dismissNotice:
                state.notice = nil
                state.noticeIsMutationFailure = false
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
        state.workspace = nil
        state.items = []
        state.hiddenIDs = []
        state.pendingIDs = []
        state.deferredReadIDs = []
        state.uncertainByItem = [:]
        state.cursor = nil
        state.presentedID = nil
        state.notice = nil
        state.noticeIsMutationFailure = false
        state.hasSnapshot = false
        state.connection = .connecting
    }

    /// Only an information root offered `mark_read` by the server leaves the queue by reading.
    /// Questions, deliveries and pull requests leave only through their own decisions.
    private func readIntent(for item: DomainV2.InboxItem) -> DomainIntent? {
        guard case .interaction(let id) = item.subject,
              item.availableActions.contains(where: { $0.actionKind == .markRead && $0.enabled }) else { return nil }
        return .markRead(id)
    }
}
