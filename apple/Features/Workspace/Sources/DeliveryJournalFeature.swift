import AlondraCommands
import AlondraContracts
import AlondraRecovery
import ComposableArchitecture
import Foundation

public struct DeliveryJournalClient: Sendable {
    public var available: Bool
    public var refresh: @Sendable () async throws -> DeliveryJournalSnapshot
    public var resolve: @Sendable (DeliveryAttempt, CommandScope, Bool, Bool, Bool) async throws -> DeliveryJournalSnapshot
    public init(available: Bool = true, refresh: @escaping @Sendable () async throws -> DeliveryJournalSnapshot,
                resolve: @escaping @Sendable (DeliveryAttempt, CommandScope, Bool, Bool, Bool) async throws -> DeliveryJournalSnapshot) {
        self.available = available; self.refresh = refresh; self.resolve = resolve
    }
}
private enum DeliveryJournalKey: DependencyKey {
    static let liveValue = DeliveryJournalClient(available: false, refresh: { throw DeliveryJournalError.obsolete }, resolve: { _, _, _, _, _ in throw DeliveryJournalError.obsolete })
}
public extension DependencyValues {
    var deliveryJournal: DeliveryJournalClient {
        get { self[DeliveryJournalKey.self] }
        set { self[DeliveryJournalKey.self] = newValue }
    }
}

@Reducer public struct DeliveryJournalFeature {
    public init() {}
    @ObservableState public struct State: Equatable {
        public var snapshot: DeliveryJournalSnapshot?
        public var error: String?
        public var presented = false
        public var selected: DeliveryAttempt?
        public var terminalChecked = false
        public var duplicateRisk = false
        public var resolving = false
        public var epoch: UInt64 = 0
        public var active = false
        public var loading = false
        public var refreshRequested = false
        public var lastTick: UInt64?
        public init() {}
        public var pending: [DeliveryAttempt] { snapshot?.attempts.filter(\.needsAttention) ?? [] }
    }
    public enum Action {
        case appeared, disappeared, refresh
        case syncTick(UInt64)
        case loaded(UInt64, DeliveryJournalSnapshot)
        case failed(UInt64, String)
        case present(Bool), select(UInt64?)
        case terminalChecked(Bool), duplicateRisk(Bool), resolve(retry: Bool)
        case resolved(UInt64, DeliveryJournalSnapshot)
        case resolutionFailed(UInt64, String)
    }
    @Dependency(\.deliveryJournal) var client
    private enum CancelID { case observation, resolution }
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .appeared:
                guard client.available else { return .none }
                state.epoch &+= 1; state.active = true; state.loading = false
                return .send(.refresh)
            case .disappeared:
                state.active = false; state.epoch &+= 1; state.resolving = false
                state.loading = false; state.refreshRequested = false
                state.selected = nil; state.presented = false
                return .merge(.cancel(id: CancelID.observation), .cancel(id: CancelID.resolution))
            case .syncTick(let tick):
                guard state.active, state.lastTick != tick else { return .none }
                state.lastTick = tick
                return .send(.refresh)
            case .refresh:
                guard state.active else { return .none }
                guard !state.loading, !state.resolving else { state.refreshRequested = true; return .none }
                state.loading = true; state.refreshRequested = false
                let epoch = state.epoch
                return .run { send in
                    do {
                        let snapshot = try await client.refresh()
                        try Task.checkCancellation()
                        await send(.loaded(epoch, snapshot))
                    } catch is CancellationError { }
                    catch { await send(.failed(epoch, error.localizedDescription)) }
                }.cancellable(id: CancelID.observation, cancelInFlight: true)
            case .loaded(let epoch, let snapshot):
                guard state.active, state.epoch == epoch else { return .none }
                state.loading = false
                state.error = nil
                if state.snapshot?.scope != snapshot.scope { state.selected = nil; state.terminalChecked = false; state.duplicateRisk = false }
                state.snapshot = snapshot
                // Keep a selection's observed version fixed. A new version needs a new decision.
                if let selected = state.selected, !snapshot.attempts.contains(selected) {
                    state.selected = nil; state.terminalChecked = false; state.duplicateRisk = false
                    state.error = DeliveryJournalError.changed.localizedDescription
                }
                return state.refreshRequested ? .send(.refresh) : .none
            case .failed(let epoch, let message):
                guard state.active, state.epoch == epoch else { return .none }
                state.error = message; state.loading = false
                return state.refreshRequested ? .send(.refresh) : .none
            case .present(let value): state.presented = value; return .none
            case .select(let id):
                guard !state.resolving else { return .none }
                state.selected = id.flatMap { id in state.pending.first { $0.id == id } }
                state.terminalChecked = false; state.duplicateRisk = false; state.error = nil
                return .none
            case .terminalChecked(let value): state.terminalChecked = value; return .none
            case .duplicateRisk(let value): state.duplicateRisk = value; return .none
            case .resolve(let retry):
                guard state.active, !state.resolving, let selected = state.selected, selected.canResolve,
                      let snapshot = state.snapshot, state.terminalChecked, !retry || state.duplicateRisk else { return .none }
                state.epoch &+= 1; state.loading = false
                let epoch = state.epoch
                state.resolving = true; state.error = nil
                return .merge(.cancel(id: CancelID.observation), .run { send in
                    do {
                        let next = try await client.resolve(selected, snapshot.scope, retry, true, retry)
                        try Task.checkCancellation()
                        await send(.resolved(epoch, next))
                    } catch is CancellationError { }
                    catch { await send(.resolutionFailed(epoch, error.localizedDescription)) }
                }.cancellable(id: CancelID.resolution, cancelInFlight: true))
            case .resolutionFailed(let epoch, let message):
                guard state.active, state.epoch == epoch else { return .none }
                state.resolving = false; state.error = message
                state.selected = nil; state.terminalChecked = false; state.duplicateRisk = false
                return .none
            case .resolved(let epoch, let snapshot):
                guard state.active, state.epoch == epoch else { return .none }
                state.resolving = false; state.selected = nil
                state.terminalChecked = false; state.duplicateRisk = false
                state.snapshot = snapshot
                return state.refreshRequested ? .send(.refresh) : .none
            }
        }
    }
}
