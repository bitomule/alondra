import AlondraTriageFeature
import ComposableArchitecture

/// Interaction state; geometry measurements and animation completion remain platform adapters.
@Reducer
public struct CardStackFeature {
    public init() {}
    public struct Departure: Equatable, Sendable {
        public let itemID: String
        public let identity: TriageIdentity?
        public let offset: Double
    }
    @ObservableState
    public struct State: Equatable {
        public init() {}
        public var drag: Double = 0
        public var departure: Departure?
        public var frontID: String?
        public var count = 0
        public var identity: TriageIdentity?
    }
    public enum Action: Sendable {
        case dragged(width: Double, height: Double)
        case dragEnded(projectedWidth: Double)
        case skipTapped(String)
        case animateDeparture(Departure)
        case resetDrag(Departure)
        case animationCompleted(Departure)
        case delegate(Departure)
    }
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case let .dragged(width, height):
                guard state.departure == nil, state.frontID != nil, state.count > 1,
                      abs(width) > abs(height) * 1.4 else { return .none }
                state.drag = width
            case let .dragEnded(projected):
                guard state.departure == nil, let id = state.frontID, state.count > 1 else { return .none }
                guard abs(state.drag) > 90 || (abs(projected) > 240 && abs(state.drag) > 30) else {
                    state.drag = 0
                    return .none
                }
                state.departure = Departure(itemID: id, identity: state.identity, offset: state.drag > 0 ? 420 : -420)
            case let .skipTapped(id):
                guard state.departure == nil, state.frontID == id else { return .none }
                state.departure = Departure(itemID: id, identity: state.identity, offset: state.drag > 0 ? 420 : -420)
            case let .animateDeparture(departure):
                guard state.departure == departure else { return .none }
                state.drag = departure.offset
            case let .resetDrag(departure):
                guard state.departure == departure else { return .none }
                state.drag = 0
            case let .animationCompleted(departure):
                guard state.departure == departure else { return .none }
                state.departure = nil
                return .send(.delegate(departure))
            case .delegate: break
            }
            return .none
        }
    }
}
