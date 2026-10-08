import AlondraContract
import ComposableArchitecture
import Foundation

/// Waiting for You: the ordered queue from the `home` view, and the cards around the one in front.
/// The server decides what is waiting and in which order. Swiping a card either way only moves on:
/// it goes to the back of the queue and nothing is answered, accepted, marked read or sent; answers
/// travel only with a tap on a button. A card answered here or from its own screen leaves at once and comes back to the front if the daemon does not apply the answer.
@Reducer
public struct TriageFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public init() {}
        public var screen = ScreenFeature.State(key: .home)
        public var cards: IdentifiedArrayOf<TriageCardFeature.State> = []
        public var orderedIDs: [String] = []
        public var index = 0
        public var machinePresented = false
        public var focusedID: String?
        public var newerQueueWaiting = false
        public var swipe: Swipe?
        public var skips = 0
        public var optimistic = Optimistic()
        /// Cards the daemon applied an answer to. They stay out of the queue until a view no longer lists them or `answeredTimeToLive` passes.
        public var answered: Set<String> = []

        public var currentID: String? { orderedIDs.indices.contains(index) ? orderedIDs[index] : nil }
        public var currentCard: Card? { currentID.flatMap { cards[id: $0]?.card } }
        public var visibleIDs: [String] {
            guard !orderedIDs.isEmpty else { return [] }
            return (0 ..< min(TriageFeature.depth, orderedIDs.count)).map { orderedIDs[(index + $0) % orderedIDs.count] }
        }
        public var status: ViewStatus? { screen.document?.status }
        /// The cards the queue shows, so the tab and the app icon never count one the queue hides.
        public var badge: Int { orderedIDs.count }
        /// Nil until the first queue arrives, so a fresh launch keeps the number a push set.
        public var iconBadge: Int? { screen.document == nil ? nil : badge }
        public var queueTitle: String? { screen.document?.sections.first { $0.id == "queue" }?.title }
        /// The header's numbers, counted over the cards the queue shows, so it never states one it
        /// hides. Nil from a daemon that does not say what each card waits for.
        public var header: Header? {
            guard let status, let queue = screen.document?.sections.first(where: { $0.id == "queue" }),
                  let ids = queue.cardIds, let waits = queue.cardWaits, waits.count == ids.count else { return nil }
            let waiting = Dictionary(zip(ids, waits), uniquingKeysWith: { first, _ in first })
            let questions = orderedIDs.filter { waiting[$0] == "question" }.count
            return Header(
                questions: questions, toRead: orderedIDs.count - questions,
                working: Int(status.counts.first { $0.id == "working" }?.value ?? 0)
            )
        }
        public var isOffline: Bool {
            if case .failed = screen.phase { return screen.document != nil }
            return false
        }
        public var loadError: String? {
            guard screen.document == nil else { return nil }
            switch screen.phase {
            case .failed(let message): return message
            case .missing: return TriageFeature.loadFailure
            case .loading, .loaded: return nil
            }
        }

        fileprivate var stampShowing: Bool { visibleIDs.contains { cards[id: $0]?.actions.stamp != nil } }
    }

    public struct Header: Equatable, Sendable {
        public let questions: Int
        public let toRead: Int
        public let working: Int
        public var waiting: Int { questions + toRead }
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
        case screen(ScreenFeature.Action)
        case cards(IdentifiedActionOf<TriageCardFeature>)
        case next
        case leaveForLater
        case dragChanged
        case dragEnded(translation: Double, predicted: Double)
        case swipeSettled
        case machinePresented(Bool)
        case stampElapsed
        case loadTimedOut
        /// A screen opened from a card sent its answer and left; the app delivers it.
        case handedOff(ActionRequest, cardID: String)
        case handOffSettled(ActionRequest, applied: Bool)
        /// A screen's answer got no reply: its card comes back saying why, ready to send the same again.
        case handOffUndelivered(ActionRequest, SendFailure)
        case sendTimedOut(ActionRequest)
        case answeredExpired(String)
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable {
            case open(ViewKey)
            /// A card opened from a notification, or shown in the queue, no longer waits: why, when
            /// the daemon said.
            case noLongerPending(String?)
        }
    }

    /// The front card and the two drawn behind it, so the next card already has its content.
    public static let depth = 3
    public static let leaveDuration = Duration.milliseconds(160)
    public static let stampDuration = Duration.milliseconds(900)
    /// How long an answered card stays hidden when no view ever stops listing it; then the daemon's word rules.
    public static let answeredTimeToLive = Duration.seconds(120)
    /// An answer the daemon has not acknowledged by then is rolled back with an error.
    public static let sendDeadline = loadDeadline
    /// Past the transport's own request timeouts: a queue still not here by then shows an error with retry.
    public static let loadDeadline = Duration.seconds(20)

    @Dependency(\.continuousClock) var clock
    @Dependency(\.viewsClient) var viewsClient
    private enum CancelID: Hashable { case stamp, swipe, loadDeadline, sendDeadline(String), answered(String) }
    public static var loadFailure: String { String(localized: "Couldn’t load this from your Mac. Try again.") }

    public static func commits(translation: Double, predicted: Double) -> Bool {
        abs(translation) > 90 || (abs(predicted) > 240 && abs(translation) > 30)
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .merge([.send(.screen(.task)), state.stampShowing ? stampTimer() : .none] + state.cards.ids.map {
                    .send(.cards(.element(id: $0, action: .screen(.task))))
                })
            case .disappeared:
                state.swipe = nil
                return .merge([
                    .send(.screen(.stop)), .cancel(id: CancelID.stamp), .cancel(id: CancelID.swipe), .cancel(id: CancelID.loadDeadline),
                ] + state.cards.ids.map {
                    .send(.cards(.element(id: $0, action: .screen(.stop))))
                })
            case .focus(let id):
                state.focusedID = id
                state.screen = ScreenFeature.State(key: .focusedHome(id))
                state.orderedIDs = []
                state.index = 0
                let optimistic = state.optimistic
                state.cards.removeAll { !optimistic.isSending($0.id) }
                state.newerQueueWaiting = false
                return .send(.screen(.task))
            case .screen(.task), .screen(.retry):
                guard state.screen.document == nil else { return .none }
                return .run { [clock] send in
                    try await clock.sleep(for: Self.loadDeadline)
                    await send(.loadTimedOut)
                }
                .cancellable(id: CancelID.loadDeadline, cancelInFlight: true)
            case .loadTimedOut:
                guard state.screen.document == nil, state.screen.phase == .loading else { return .none }
                state.screen.phase = .failed(Self.loadFailure)
                return .none
            case .screen(.missing) where state.screen.key != .home:
                state.screen = ScreenFeature.State(key: .home)
                return .send(.screen(.task))
            case .screen(.loaded(let document)):
                guard document.key == state.screen.key.rawValue else { return .none }
                state.optimistic.reconcile(document.version)
                let listed = Set(document.sections.first { $0.id == "queue" }?.cardIds ?? [])
                let gone = state.answered.subtracting(listed)
                state.answered.subtract(gone)
                let forgotten: Effect<Action> = .merge(gone.map { .cancel(id: CancelID.answered($0)) })
                if state.stampShowing {
                    state.newerQueueWaiting = true
                    return .merge(.cancel(id: CancelID.loadDeadline), forgotten)
                }
                return .merge(.cancel(id: CancelID.loadDeadline), forgotten, showQueue(&state))
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
                guard Self.commits(translation: translation, predicted: predicted) else { return settle() }
                let direction: Direction = translation > 0 ? .right : .left
                if state.orderedIDs.count > 1 {
                    state.swipe?.leaving = direction
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
            case .cards(.element(id: let id, action: .delegate(.settled(let why)))):
                guard !state.answered.contains(id), !state.optimistic.hides(id) else { return .none }
                state.answered.insert(id)
                return .merge(showQueue(&state), .send(.delegate(.noLongerPending(why))), answeredTimer(id))
            case .cards(.element(id: _, action: .delegate(.open(let key)))):
                guard state.swipe == nil else { return .none }
                return .send(.delegate(.open(key)))
            case .cards(.element(id: let id, action: .actions(.sent(let request)))):
                guard request.resolvesItsCard else { return stampTimer() }
                state.optimistic.begin(id, request)
                return .merge(stampTimer(), sendDeadline(request))
            case .cards(.element(id: _, action: .actions(.settled(let request, let applied)))),
                 .handOffSettled(let request, let applied):
                return settle(request, applied: applied, &state)
            case .handOffUndelivered(let request, let failure):
                let id = state.optimistic.target(of: request)
                let rolledBack = settle(request, applied: false, &state)
                if let id, state.cards[id: id] != nil {
                    state.cards[id: id]?.actions.error = failure.message
                    state.cards[id: id]?.actions.unsure = failure == .unsure ? request : nil
                }
                return rolledBack
            case .sendTimedOut(let request):
                guard let id = state.optimistic.target(of: request) else { return .none }
                let rolledBack = settle(request, applied: false, &state)
                guard state.cards[id: id] != nil else { return rolledBack }
                return .merge(
                    .cancel(id: ActionFeature.DeliveryID(commandID: request.commandId)),
                    rolledBack,
                    .send(.cards(.element(id: id, action: .actions(.undelivered(request, .unsure)))))
                )
            case .answeredExpired(let id):
                guard state.answered.remove(id) != nil else { return .none }
                let queue = showQueue(&state)
                guard state.cards[id: id] != nil else { return queue }
                return .merge(queue, .send(.cards(.element(id: id, action: .actions(.dismiss)))))
            case .handedOff(let request, let cardID):
                guard request.resolvesItsCard else { return .none }
                state.optimistic.begin(cardID, request)
                guard !state.stampShowing else {
                    state.newerQueueWaiting = true
                    return sendDeadline(request)
                }
                return .merge(showQueue(&state), sendDeadline(request))
            case .stampElapsed:
                state.newerQueueWaiting = false
                let queue = showQueue(&state)
                let stamped = state.cards.filter { $0.actions.stamp != nil && !state.optimistic.isSending($0.id) }.map(\.id)
                return .merge([queue, .send(.screen(.retry))] + stamped.map {
                    .send(.cards(.element(id: $0, action: .actions(.dismiss))))
                })
            case .cards, .delegate:
                return .none
            }
        }
        .forEach(\.cards, action: \.cards) { TriageCardFeature() }
    }

    private func showQueue(_ state: inout State, front: String? = nil) -> Effect<Action> {
        guard let document = state.screen.document else { return .none }
        let previousID = front ?? state.currentID
        let ids = (document.sections.first { $0.id == "queue" }?.cardIds ?? []).filter { !state.optimistic.hides($0) && !state.answered.contains($0) }
        state.orderedIDs = ids
        state.index = previousID.flatMap(ids.firstIndex(of:)) ?? min(state.index, max(ids.count - 1, 0))
        var effects = [keepWindowObserved(&state)]
        if let focused = state.focusedID {
            state.focusedID = nil
            if ids.first != focused {
                state.index = min(state.index, max(ids.count - 1, 0))
                effects.append(.run { [viewsClient] send in
                    let fetch = try? await viewsClient.load(.card(focused), nil)
                    guard case .document(let document)? = fetch else { return await send(.delegate(.noLongerPending(nil))) }
                    await send(.delegate(.noLongerPending(document.settledNotice)))
                })
            }
        }
        return .merge(effects)
    }

    /// An answer the daemon did not apply: its card returns to the front, where it was answered,
    /// keeping what it showed and what it sent.
    private func bringBack(_ id: String, _ state: inout State) -> Effect<Action> {
        showQueue(&state, front: id)
    }

    /// The answer is final once the daemon applied it: the card stays out, the queue is pulled again
    /// even when no push comes, and a card no view ever stops listing returns after the time to live.
    private func settle(_ request: ActionRequest, applied: Bool, _ state: inout State) -> Effect<Action> {
        let target = state.optimistic.target(of: request)
        let returned = state.optimistic.settle(request, applied: applied, shown: state.screen.document?.version)
        let deadline: Effect<Action> = .cancel(id: CancelID.sendDeadline(request.commandId))
        if let returned { return .merge(deadline, bringBack(returned, &state)) }
        guard applied, let target else { return deadline }
        state.answered.insert(target)
        return .merge(deadline, .send(.screen(.retry)), answeredTimer(target))
    }

    private func sendDeadline(_ request: ActionRequest) -> Effect<Action> {
        .run { [clock] send in
            try await clock.sleep(for: Self.sendDeadline)
            await send(.sendTimedOut(request))
        }
        .cancellable(id: CancelID.sendDeadline(request.commandId), cancelInFlight: true)
    }

    private func answeredTimer(_ id: String) -> Effect<Action> {
        .run { [clock] send in
            try await clock.sleep(for: Self.answeredTimeToLive)
            await send(.answeredExpired(id))
        }
        .cancellable(id: CancelID.answered(id), cancelInFlight: true)
    }

    private func stampTimer() -> Effect<Action> {
        .run { [clock] send in
            try await clock.sleep(for: Self.stampDuration)
            await send(.stampElapsed)
        }
        .cancellable(id: CancelID.stamp, cancelInFlight: true)
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
        let optimistic = state.optimistic
        state.cards.removeAll { !wanted.contains($0.id) && !optimistic.isSending($0.id) }
        let epoch = state.screen.document?.dataEpoch ?? ""
        var started: [Effect<Action>] = []
        for id in wanted where state.cards[id: id] == nil {
            state.cards.append(TriageCardFeature.State(id: id, dataEpoch: epoch))
            started.append(.send(.cards(.element(id: id, action: .screen(.task)))))
        }
        return .merge(started)
    }
}
