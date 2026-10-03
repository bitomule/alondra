import AlondraContract
import ComposableArchitecture
import Foundation

/// Waiting for You: the ordered queue from the `home` view, and the cards around the one in front.
/// The server decides what is waiting and in which order; swiping only browses.
@Reducer
public struct TriageFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public init() {}
        public var screen = ScreenFeature.State(key: .home)
        public var status: ViewStatus?
        public var cards: IdentifiedArrayOf<TriageCardFeature.State> = []
        public var orderedIDs: [String] = []
        public var index = 0
        public var machinePresented = false
        public var focusedID: String?
        public var newerQueueWaiting = false

        public var currentID: String? { orderedIDs.indices.contains(index) ? orderedIDs[index] : nil }
        public var currentCard: Card? { currentID.flatMap { cards[id: $0]?.card } }
        public var badge: Int { iconBadge ?? 0 }
        /// What waits for the user, as the server counts it; nil until the first status arrives.
        public var iconBadge: Int? { (status?.badge ?? screen.document?.status.badge).map(Int.init) }
        public var queueTitle: String? { screen.document?.sections.first { $0.id == "queue" }?.title }
        public var isOffline: Bool {
            if case .failed = screen.phase { return screen.document != nil }
            return false
        }
        public var loadError: String? {
            if case .failed(let message) = screen.phase, screen.document == nil { return message }
            return nil
        }

        fileprivate var stampShowing: Bool { cards.contains { $0.actions.stamp != nil } }
    }

    public enum Action {
        case task
        case disappeared
        case focus(String)
        case feedStatus(ViewStatus)
        case screen(ScreenFeature.Action)
        case cards(IdentifiedActionOf<TriageCardFeature>)
        case move(Int)
        case machinePresented(Bool)
        case stampElapsed
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable {
            case open(ViewKey)
            case noLongerPending
        }
    }

    /// Cards kept alive around the front one: the previous so a swipe back is instant, the next so
    /// a swipe forward already has its content.
    static let window = -1 ... 1
    static let stampDuration = Duration.milliseconds(650)

    @Dependency(\.continuousClock) var clock
    private enum CancelID { case stamp }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .merge([.send(.screen(.task))] + state.cards.ids.map {
                    .send(.cards(.element(id: $0, action: .screen(.task))))
                })
            case .disappeared:
                return .merge([.send(.screen(.stop)), .cancel(id: CancelID.stamp)] + state.cards.ids.map {
                    .send(.cards(.element(id: $0, action: .screen(.stop))))
                })
            case .focus(let id):
                state.focusedID = id
                state.screen = ScreenFeature.State(key: .focusedHome(id))
                state.orderedIDs = []
                state.index = 0
                state.cards = []
                state.newerQueueWaiting = false
                return .send(.screen(.task))
            case .feedStatus(let status):
                state.status = status
                return .none
            case .screen(.loaded(let document)):
                guard document.key == state.screen.key.rawValue else { return .none }
                state.status = document.status
                if state.stampShowing {
                    state.newerQueueWaiting = true
                    return .none
                }
                return showQueue(&state)
            case .screen:
                return .none
            case .move(let delta):
                guard !state.orderedIDs.isEmpty else { return .none }
                let previous = state.index
                state.index = min(max(state.index + delta, 0), state.orderedIDs.count - 1)
                guard previous != state.index else { return .none }
                return keepWindowObserved(&state)
            case .machinePresented(let presented):
                state.machinePresented = presented
                return .none
            case .cards(.element(id: _, action: .delegate(.open(let key)))):
                return .send(.delegate(.open(key)))
            case .cards(.element(id: _, action: .actions(.response(let response)))):
                guard response.outcome == .applied else { return .none }
                return .run { [clock] send in
                    try await clock.sleep(for: Self.stampDuration)
                    await send(.stampElapsed)
                }
                .cancellable(id: CancelID.stamp, cancelInFlight: true)
            case .stampElapsed:
                state.newerQueueWaiting = false
                let queue = showQueue(&state)
                let stamped = state.cards.filter { $0.actions.stamp != nil }.map(\.id)
                return .merge([queue, .send(.screen(.retry))] + stamped.map {
                    .send(.cards(.element(id: $0, action: .actions(.dismiss))))
                })
            case .cards, .delegate:
                return .none
            }
        }
        .forEach(\.cards, action: \.cards) { TriageCardFeature() }
    }

    private func showQueue(_ state: inout State) -> Effect<Action> {
        guard let document = state.screen.document else { return .none }
        let previousID = state.currentID
        let ids = document.sections.first { $0.id == "queue" }?.cardIds ?? []
        state.orderedIDs = ids
        state.index = previousID.flatMap(ids.firstIndex(of:)) ?? min(state.index, max(ids.count - 1, 0))
        var effects = [keepWindowObserved(&state)]
        if let focused = state.focusedID {
            state.focusedID = nil
            if ids.first != focused {
                state.index = min(state.index, max(ids.count - 1, 0))
                effects.append(.send(.delegate(.noLongerPending)))
            }
        }
        return .merge(effects)
    }

    private func keepWindowObserved(_ state: inout State) -> Effect<Action> {
        let wanted = Self.window.map { state.index + $0 }
            .filter { state.orderedIDs.indices.contains($0) }
            .map { state.orderedIDs[$0] }
        state.cards.removeAll { !wanted.contains($0.id) }
        let epoch = state.screen.document?.dataEpoch ?? ""
        var started: [Effect<Action>] = []
        for id in wanted where state.cards[id: id] == nil {
            state.cards.append(TriageCardFeature.State(id: id, dataEpoch: epoch))
            started.append(.send(.cards(.element(id: id, action: .screen(.task)))))
        }
        return .merge(started)
    }
}
