import AlondraTasksFeature
import AlondraFleetFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture

@Reducer
public struct ServiceFleetFeature {
    public init() {}
    @Reducer
    public enum Path {
        case node(NodeDetailFeature)
        case task(TaskDetailFeature)
    }
    @ObservableState
    public struct State {
        public init() {}
        public var fleet = FleetFeature.State()
        public var nodePath: [String] = []
        public var path = StackState<Path.State>()
        public var presentation = ServiceTaskPresentation(overview: nil)
        @Presents public var taskDetail: TaskDetailFeature.State?
        public var focusedInboxID: String?
        public var nodes: IdentifiedArrayOf<NodeDetailFeature.State> = []
        public var identity: TriageIdentity?
        public mutating func resetIdentity(_ identity: TriageIdentity?) {
            guard self.identity != identity else { return }
            self.identity = identity
            nodes = []
            detailID = nil
        }
        public var detailID: String?
        public var detail: NodeDetailFeature.State? {
            get { detailID.flatMap { nodes[id: $0] } }
            set {
                if let newValue { nodes[id: newValue.id] = newValue; detailID = newValue.id }
                else { detailID = nil }
            }
        }
        public mutating func cancelPendingSubmissions() {
            for id in nodes.ids { nodes[id: id]?.pendingSubmission = nil }
        }
        public var sequence: UInt64 = 0
        public var overview: Overview?
        public var hiddenInboxIDs: Set<String> = []
    }
    public enum Action {
        case appeared, disappeared, refresh
        case openTask(String), focusInbox(String), inboxFocusHandled
        case taskDetail(PresentationAction<TaskDetailFeature.Action>)
        case snapshot(Overview?)
        case hiddenInboxChanged(Set<String>)
        case fleet(FleetFeature.Action)
        case detailDismissed
        case nodes(IdentifiedActionOf<NodeDetailFeature>)
        case openNode(String)
        case nodePathChanged([String])
        case path(StackActionOf<Path>)
        case pushTask(String)
    }
    @Dependency(\.serviceTasksClient) var snapshots
    private enum CancelID { case observation }
    public var body: some ReducerOf<Self> {
        Scope(state: \.fleet, action: \.fleet) { FleetFeature() }
        Reduce { state, action in
            switch action {
            case .pushTask(let id):
                guard let task = state.presentation.board.task(id) else { return .none }
                state.path.append(.task(TaskDetailFeature.State(taskID: id)))
                guard let pathID = state.path.ids.last else { return .none }
                return .send(.path(.element(id: pathID, action: .task(.taskChanged(task)))))
            case .path(.push(let id, _)):
                guard case .node = state.path[id: id], let snapshot = state.fleet.snapshot else { return .none }
                return .send(.path(.element(id: id, action: .node(.snapshotChanged(snapshot)))))
            case .path(.element(let id, .node(.closeFinished))):
                if case .node(let node) = state.path[id: id], node.shouldDismiss {
                    state.path.pop(from: id)
                }
                return .none
            case .refresh: return .run { _ in await snapshots.refresh() }
            case .openTask(let id):
                guard let task = state.presentation.board.task(id) else { return .none }
                state.taskDetail = TaskDetailFeature.State(taskID: id)
                return .send(.taskDetail(.presented(.taskChanged(task))))
            case .focusInbox(let id): state.focusedInboxID = id; return .none
            case .inboxFocusHandled: state.focusedInboxID = nil; return .none
            case .appeared:
                return .run { send in
                    for await snapshot in await snapshots.observe() { await send(.snapshot(snapshot)) }
                }.cancellable(id: CancelID.observation, cancelInFlight: true)
            case .disappeared:
                state.cancelPendingSubmissions()
                return .cancel(id: CancelID.observation)
            case .snapshot(let overview):
                state.overview = overview
                return project(&state)
            case .hiddenInboxChanged(let ids):
                guard state.hiddenInboxIDs != ids else { return .none }
                state.hiddenInboxIDs = ids
                return project(&state)
            case .nodePathChanged(let path):
                state.nodePath = path
                guard let id = path.last else { state.detail = nil; return .none }
                return .send(.openNode(id))
            case .openNode(let id):
                guard let snapshot = state.fleet.snapshot, snapshot.node(id) != nil else { return .none }
                state.cancelPendingSubmissions()
                if state.nodes[id: id] == nil { state.nodes.append(NodeDetailFeature.State(sessionID: id)) }
                state.detailID = id
                return .send(.nodes(.element(id: id, action: .snapshotChanged(snapshot))))
            case .detailDismissed:
                if let id = state.detailID { state.nodes[id: id]?.pendingSubmission = nil }
                state.detailID = nil
                return .none
            case .nodes(.element(let id, .closeFinished)), .nodes(.element(let id, .commandResolved)):
                if state.nodes[id: id]?.shouldDismiss == true {
                    state.nodes.remove(id: id)
                    if state.detailID == id { state.detailID = nil; state.nodePath = [] }
                }
                return .none
            case .fleet, .nodes, .taskDetail, .path: return .none
            }
        }.forEach(\.nodes, action: \.nodes) { NodeDetailFeature() }
            .ifLet(\.$taskDetail, action: \.taskDetail) { TaskDetailFeature() }
            .forEach(\.path, action: \.path)
    }

    private func project(_ state: inout State) -> Effect<Action> {
        state.presentation = ServiceTaskPresentation(overview: state.overview, hiddenIDs: state.hiddenInboxIDs)
        let taskUpdate: Effect<Action> = state.taskDetail.map { detail in
            .send(.taskDetail(.presented(.taskChanged(state.presentation.board.task(detail.taskID)))))
        } ?? .none
        guard let overview = state.overview else {
            let pathUpdates: Effect<Action> = .merge(state.path.ids.map { id in
                switch state.path[id: id]! {
                case .node: return .send(.path(.element(id: id, action: .node(.snapshotUnavailable))))
                case .task: return .send(.path(.element(id: id, action: .task(.taskChanged(nil)))))
                }
            })
            return .merge(pathUpdates, taskUpdate, .send(.fleet(.snapshotUnavailable)), .merge(state.nodes.ids.map { .send(.nodes(.element(id: $0, action: .snapshotUnavailable))) }))
        }
        state.sequence &+= 1
        let ids = Set(ObserveInboxUseCase().execute(overview).items.map(\.id)).subtracting(state.hiddenInboxIDs)
        let snapshot = ProjectFleetUseCase().execute(overview, visibleInboxIDs: ids, sequence: state.sequence)
        let pathUpdates: Effect<Action> = .merge(state.path.ids.map { id in
            switch state.path[id: id]! {
            case .node: return .send(.path(.element(id: id, action: .node(.snapshotChanged(snapshot)))))
            case .task(let task): return .send(.path(.element(id: id, action: .task(.taskChanged(state.presentation.board.task(task.taskID))))))
            }
        })
        return .merge(pathUpdates, taskUpdate, .send(.fleet(.snapshotChanged(snapshot))), .merge(state.nodes.ids.map { .send(.nodes(.element(id: $0, action: .snapshotChanged(snapshot)))) }))
    }
}
