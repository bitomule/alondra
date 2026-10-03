import AlondraContract
import ComposableArchitecture
import Foundation

/// Waiting for You: the ordered queue from the `home` view, and the cards around the one in front.
/// The server decides what is waiting and in which order. Swiping either way sends the front card to
/// the back of the queue and marks nothing.
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
        public var swipe: Swipe?
        public var skips = 0

        public var currentID: String? { orderedIDs.indices.contains(index) ? orderedIDs[index] : nil }
        public var currentCard: Card? { currentID.flatMap { cards[id: $0]?.card } }
        public var visibleIDs: [String] {
            guard !orderedIDs.isEmpty else { return [] }
            return (0 ..< min(TriageFeature.depth, orderedIDs.count)).map { orderedIDs[(index + $0) % orderedIDs.count] }
        }
        public var badge: Int { Int(status?.badge ?? screen.document?.status.badge ?? 0) }
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

    /// A drag on the front card, from its first movement until the card has left or sprung back.
    public struct Swipe: Equatable, Sendable {
        public let cardID: String
        public var released = false
        public var leaving: Direction?

        public init(cardID: String, released: Bool = false, leaving: Direction? = nil) {
            self.cardID = cardID
            self.released = released
            self.leaving = leaving
        }
    }

    public enum Direction: Equatable, Sendable {
        case left, right
    }

    public enum Action {
        case task
        case disappeared
        case focus(String)
        case feedStatus(ViewStatus)
        case screen(ScreenFeature.Action)
        case cards(IdentifiedActionOf<TriageCardFeature>)
        case next
        case leaveForLater
        case dragChanged
        case dragEnded(translation: Double, predicted: Double)
        case swipeSettled
        case machinePresented(Bool)
        case stampElapsed
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable {
            case open(ViewKey)
            case noLongerPending
        }
    }

    /// The front card and the two drawn behind it, so the next card already has its content.
    public static let depth = 3
    public static let leaveDuration = Duration.milliseconds(160)
    static let stampDuration = Duration.milliseconds(650)

    @Dependency(\.continuousClock) var clock
    private enum CancelID { case stamp, swipe }

    public static func commits(translation: Double, predicted: Double) -> Bool {
        abs(translation) > 90 || (abs(predicted) > 240 && abs(translation) > 30)
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .merge([.send(.screen(.task))] + state.cards.ids.map {
                    .send(.cards(.element(id: $0, action: .screen(.task))))
                })
            case .disappeared:
                state.swipe = nil
                return .merge([.send(.screen(.stop)), .cancel(id: CancelID.stamp), .cancel(id: CancelID.swipe)] + state.cards.ids.map {
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
            case .next:
                return advance(&state)
            case .leaveForLater:
                guard state.swipe == nil, state.orderedIDs.count > 1, let id = state.currentID else { return .none }
                state.swipe = Swipe(cardID: id, released: true, leaving: .left)
                return settle()
            case .dragChanged:
                guard state.swipe == nil, let id = state.currentID else { return .none }
                state.swipe = Swipe(cardID: id)
                return .none
            case .dragEnded(let translation, let predicted):
                guard state.swipe?.released == false else { return .none }
                state.swipe?.released = true
                if state.orderedIDs.count > 1, Self.commits(translation: translation, predicted: predicted) {
                    state.swipe?.leaving = translation > 0 ? .right : .left
                }
                return settle()
            case .swipeSettled:
                guard let swipe = state.swipe else { return .none }
                state.swipe = nil
                guard swipe.leaving != nil, swipe.cardID == state.currentID else { return .none }
                return advance(&state)
            case .machinePresented(let presented):
                state.machinePresented = presented
                return .none
            case .cards(.element(id: _, action: .delegate(.open(let key)))):
                guard state.swipe == nil else { return .none }
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

    private func settle() -> Effect<Action> {
        .run { [clock] send in
            try await clock.sleep(for: Self.leaveDuration)
            await send(.swipeSettled, animation: .spring(duration: 0.5, bounce: 0.2))
        }
        .cancellable(id: CancelID.swipe, cancelInFlight: true)
    }

    private func advance(_ state: inout State) -> Effect<Action> {
        guard state.orderedIDs.count > 1 else { return .none }
        state.index = (state.index + 1) % state.orderedIDs.count
        state.skips += 1
        return keepWindowObserved(&state)
    }

    private func keepWindowObserved(_ state: inout State) -> Effect<Action> {
        let wanted = state.visibleIDs
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
