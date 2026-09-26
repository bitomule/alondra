import AlondraFleetFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture

@Reducer
public struct ServiceFleetFeature {
    public init() {}
    @ObservableState
    public struct State {
        public init() {}
        public var fleet = FleetFeature.State()
        @Presents public var detail: NodeDetailFeature.State?
        public var sequence: UInt64 = 0
        public var overview: Overview?
        public var hiddenInboxIDs: Set<String> = []
    }
    public enum Action {
        case appeared, disappeared
        case snapshot(Overview?)
        case hiddenInboxChanged(Set<String>)
        case fleet(FleetFeature.Action)
        case detail(PresentationAction<NodeDetailFeature.Action>)
        case openNode(String)
    }
    @Dependency(\.serviceTasksClient) var snapshots
    private enum CancelID { case observation }
    public var body: some ReducerOf<Self> {
        Scope(state: \.fleet, action: \.fleet) { FleetFeature() }
        Reduce { state, action in
            switch action {
            case .appeared:
                return .run { send in
                    for await snapshot in await snapshots.observe() { await send(.snapshot(snapshot)) }
                }.cancellable(id: CancelID.observation, cancelInFlight: true)
            case .disappeared: return .cancel(id: CancelID.observation)
            case .snapshot(let overview):
                state.overview = overview
                return project(&state)
            case .hiddenInboxChanged(let ids):
                guard state.hiddenInboxIDs != ids else { return .none }
                state.hiddenInboxIDs = ids
                return project(&state)
            case .openNode(let id):
                guard let snapshot = state.fleet.snapshot, snapshot.node(id) != nil else { return .none }
                state.detail = NodeDetailFeature.State(sessionID: id)
                return .send(.detail(.presented(.snapshotChanged(snapshot))))
            case .detail(.presented(.closeFinished)):
                if state.detail?.shouldDismiss == true { state.detail = nil }
                return .none
            case .fleet, .detail: return .none
            }
        }.ifLet(\.$detail, action: \.detail) { NodeDetailFeature() }
    }

    private func project(_ state: inout State) -> Effect<Action> {
        guard let overview = state.overview else {
            return state.detail == nil ? .send(.fleet(.snapshotUnavailable)) :
                .merge(.send(.fleet(.snapshotUnavailable)), .send(.detail(.presented(.snapshotUnavailable))))
        }
        state.sequence &+= 1
        let ids = Set(ObserveInboxUseCase().execute(overview).items.map(\.id)).subtracting(state.hiddenInboxIDs)
        let snapshot = ProjectFleetUseCase().execute(overview, visibleInboxIDs: ids, sequence: state.sequence)
        return state.detail == nil ? .send(.fleet(.snapshotChanged(snapshot))) :
            .merge(.send(.fleet(.snapshotChanged(snapshot))), .send(.detail(.presented(.snapshotChanged(snapshot)))))
    }
}
