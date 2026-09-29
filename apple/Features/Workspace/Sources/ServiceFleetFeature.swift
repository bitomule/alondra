import AlondraContracts
import AlondraDomain
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
        case node(NodeDetailRouteFeature)
        case task(TaskDetailFeature)
    }
    @ObservableState
    public struct State {
        public init() {}
        public var fleet = FleetFeature.State()
        public var nodePath: [String] = []
        public var path = StackState<Path.State>()
        public var presentation = ServiceTaskPresentation(workspace: nil)
        @Presents public var taskDetail: TaskDetailFeature.State?
        public var focusedInboxID: String?
        public var nodes: IdentifiedArrayOf<NodeDetailFeature.State> = []
        public var identity: TriageIdentity?
        public mutating func resetIdentity(_ identity: TriageIdentity?) {
            guard self.identity != identity else { return }
            self.identity = identity
            nodes = []
            detailID = nil
            nodePath = []
            path = StackState<Path.State>()
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
        public var workspace: DomainWorkspace?
        public var hiddenInboxIDs: Set<String> = []
    }
    public enum Action {
        case appeared, disappeared, refresh
        case identityChanged(TriageIdentity)
        case openTask(DomainV2.TaskId), focusInbox(String), inboxFocusHandled
        case taskDetail(PresentationAction<TaskDetailFeature.Action>)
        case workspaceChanged(DomainWorkspace?)
        case hiddenInboxChanged(Set<String>)
        case fleet(FleetFeature.Action)
        case detailDismissed
        case nodes(IdentifiedActionOf<NodeDetailFeature>)
        case openNode(String)
        case nodePathChanged([String])
        case path(StackActionOf<Path>)
        case pushTask(DomainV2.TaskId)
    }
    @Dependency(\.serviceTasksClient) var snapshots
    public var body: some ReducerOf<Self> {
        Scope(state: \.fleet, action: \.fleet) { FleetFeature() }
        Reduce { state, action in
            switch action {
            case .identityChanged(let identity):
                state.resetIdentity(identity)
                return .none
            case .pushTask(let id):
                guard let detail = ServiceTasksFeature.detail(id, state.workspace) else { return .none }
                state.path.append(.task(TaskDetailFeature.State(taskID: id)))
                guard let pathID = state.path.ids.last else { return .none }
                return .send(.path(.element(id: pathID, action: .task(.taskChanged(detail, state.workspace)))))
            case .path(.push(let id, _)):
                guard case .node(let route) = state.path[id: id] else { return .none }
                return .send(.openNode(route.sessionID))
            case .refresh: return .run { _ in await snapshots.refresh() }
            case .openTask(let id):
                guard let detail = ServiceTasksFeature.detail(id, state.workspace) else { return .none }
                state.taskDetail = TaskDetailFeature.State(taskID: id)
                return .send(.taskDetail(.presented(.taskChanged(detail, state.workspace))))
            case .focusInbox(let id): state.focusedInboxID = id; return .none
            case .inboxFocusHandled: state.focusedInboxID = nil; return .none
            case .appeared: return .none
            case .disappeared:
                state.cancelPendingSubmissions()
                return .none
            case .workspaceChanged(let workspace):
                state.workspace = workspace
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
            case .nodes(.element(let id, .acknowledgeFinished)), .nodes(.element(let id, .cancelFinished)), .nodes(.element(let id, .retryFinished)), .nodes(.element(let id, .closeFinished)), .nodes(.element(let id, .sendFinished)):
                if let node = state.nodes[id: id], node.shouldDismiss || (
                    node.acceptedSequence != nil && node.entry == nil && !node.isSending && !node.isClosing && !node.hasOutstandingOrchestratorControl
                        && node.sendOutcome != .unknown && node.closeOutcome != .unknown
                ) {
                    state.nodes.remove(id: id)
                    if let pathID = state.path.ids.first(where: { pathID in
                        if case .node(let route) = state.path[id: pathID] { return route.sessionID == id }
                        return false
                    }) { state.path.pop(from: pathID) }
                    if state.detailID == id { state.detailID = nil; state.nodePath = [] }
                }
                return .none
            case .fleet, .nodes, .taskDetail, .path: return .none
            }
        }.forEach(\.nodes, action: \.nodes) { NodeDetailFeature() }
            .ifLet(\.$taskDetail, action: \.taskDetail) { TaskDetailFeature() }
            .forEach(\.path, action: \.path)
            .onChange(of: \.path.ids) { _, _ in
                Reduce { state, _ in
                    state.cancelPendingSubmissions()
                    return .none
                }
            }
    }

    private func project(_ state: inout State) -> Effect<Action> {
        state.presentation = ServiceTaskPresentation(workspace: state.workspace, hiddenIDs: state.hiddenInboxIDs)
        let workspace = state.workspace
        let taskUpdate: Effect<Action> = state.taskDetail.map { detail in
            .send(.taskDetail(.presented(.taskChanged(ServiceTasksFeature.detail(detail.taskID, workspace), workspace))))
        } ?? .none
        guard let workspace else {
            let pathUpdates: Effect<Action> = .merge(state.path.ids.map { id in
                switch state.path[id: id]! {
                case .node: return .none
                case .task: return .send(.path(.element(id: id, action: .task(.taskChanged(nil, nil)))))
                }
            })
            return .merge(pathUpdates, taskUpdate, .send(.fleet(.snapshotUnavailable)), .merge(state.nodes.ids.map { .send(.nodes(.element(id: $0, action: .snapshotUnavailable))) }))
        }
        state.sequence &+= 1
        let ids = Set(workspace.roots.map(\.id)).subtracting(state.hiddenInboxIDs)
        let snapshot = ProjectFleetUseCase().execute(workspace, visibleInboxIDs: ids, sequence: state.sequence)
        for node in state.nodes where snapshot.node(node.id) == nil && !node.isSending && !node.isClosing && !node.hasOutstandingOrchestratorControl
            && node.sendOutcome != .unknown && node.closeOutcome != .unknown {
            state.nodes.remove(id: node.id)
            if state.detailID == node.id { state.detailID = nil }
        }
        let pathUpdates: Effect<Action> = .merge(state.path.ids.map { id in
            switch state.path[id: id]! {
            case .node: return .none
            case .task(let task): return .send(.path(.element(id: id, action: .task(.taskChanged(ServiceTasksFeature.detail(task.taskID, workspace), workspace)))))
            }
        })
        return .merge(pathUpdates, taskUpdate, .send(.fleet(.snapshotChanged(snapshot))), .merge(state.nodes.ids.map { .send(.nodes(.element(id: $0, action: .snapshotChanged(snapshot)))) }))
    }
}

/// Navigation carries identity only. The workspace's nodes collection owns drafts and effects.
@Reducer
public struct NodeDetailRouteFeature {
    @ObservableState
    public struct State: Equatable {
        public let sessionID: String
        public init(sessionID: String) { self.sessionID = sessionID }
    }
    public enum Action {}
    public init() {}
    public var body: some ReducerOf<Self> { EmptyReducer() }
}
