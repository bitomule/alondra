import AlondraContracts
import AlondraDomain
import AlondraDomainDependency
import AlondraShared
import AlondraTasksFeature
import ComposableArchitecture
import Foundation

public struct ServiceTasksClient: Sendable {
    public init(refresh: @escaping @Sendable () async -> Void = {}) { self.refresh = refresh }
    public var refresh: @Sendable () async -> Void
}
private enum ServiceTasksClientKey: DependencyKey {
    static let liveValue = ServiceTasksClient()
    static let testValue = liveValue
}
extension DependencyValues {
    public var serviceTasksClient: ServiceTasksClient {
        get { self[ServiceTasksClientKey.self] }
        set { self[ServiceTasksClientKey.self] = newValue }
    }
}

@Reducer
public struct ServiceTasksFeature {
    public init() {}
    @ObservableState
    public struct State {
        public init() {}
        public var board = TaskBoardFeature.State()
        public var taskPath = StackState<TaskDetailFeature.State>()
        public var schedules = ScheduleFeature.State()
        public var presentation = ServiceTaskPresentation(workspace: nil)
        public var hiddenInboxIDs: Set<String> = []
        public var withdrawingRequestID: DomainV2.RequestId?
        public var isWithdrawing = false
        public var withdrawOutcome: TaskMutationOutcome?
        public var withdrawOperationRequestID: DomainV2.RequestId?
        /// Sending again after an unknown outcome reuses this ID; the server applies it once.
        public var withdrawCommandIDs: [DomainV2.RequestId: String] = [:]
        public var connection: TriageConnection = .connecting
        public var workspace: DomainWorkspace?
        public var taskBoard = TaskBoard.empty
        public var openTrees: [RequestTree] = []
        public var doneTrees: [RequestTree] = []
        public var archivedTrees: [RequestTree] = []
        @Presents public var detail: TaskDetailFeature.State?
        public var detailContext = ServiceTaskContext()
        public var showingSchedules = false
        public var deletingScheduleID: String?
        public var focusedInboxID: String?

        public func canWithdraw(_ request: DomainV2.RequestId) -> Bool {
            guard let request = taskBoard.request(request), !request.closed else { return false }
            return !(withdrawOutcome == .confirmed && withdrawOperationRequestID == request.id)
        }
    }
    public enum Action {
        case appeared, disappeared, refresh
        case withdrawRequested(DomainV2.RequestId), withdrawCancelled, withdrawConfirmed
        case withdrawFinished(DomainV2.RequestId, TaskMutationOutcome)
        case workspaceChanged(DomainWorkspace?)
        case hiddenInboxChanged(Set<String>)
        case board(TaskBoardFeature.Action)
        case schedules(ScheduleFeature.Action)
        case detail(PresentationAction<TaskDetailFeature.Action>)
        case openTask(DomainV2.TaskId)
        case pushTask(DomainV2.TaskId)
        case taskPath(StackActionOf<TaskDetailFeature>)
        case showSchedules(Bool)
        case deleteSchedule(String?)
        case confirmDeleteSchedule
        case focusInbox(String)
        case inboxFocusHandled
    }
    @Dependency(\.domainWorkspace) var domain
    @Dependency(\.serviceTasksClient) var client
    @Dependency(\.uuid) var uuid
    public var body: some ReducerOf<Self> {
        Scope(state: \.board, action: \.board) { TaskBoardFeature() }
        Scope(state: \.schedules, action: \.schedules) { ScheduleFeature() }
        Reduce { state, action in
            switch action {
            case .refresh: return .run { _ in await client.refresh() }
            case .withdrawRequested(let id):
                guard !state.isWithdrawing, state.canWithdraw(id) else { return .none }
                state.withdrawingRequestID = id
                return .none
            case .withdrawCancelled: state.withdrawingRequestID = nil; return .none
            case .withdrawConfirmed:
                guard let id = state.withdrawingRequestID, !state.isWithdrawing, state.canWithdraw(id),
                      let workspace = state.workspace else { return .none }
                state.withdrawingRequestID = nil
                state.withdrawOperationRequestID = id
                state.isWithdrawing = true
                state.withdrawOutcome = nil
                let commandID = state.withdrawCommandIDs[id] ?? uuid().uuidString.lowercased()
                state.withdrawCommandIDs[id] = commandID
                return .run { send in
                    let result = await domain.perform(.closeRequest(id), workspace, [], commandID)
                    await send(.withdrawFinished(id, TaskMutationOutcome(result)))
                }
            case .withdrawFinished(let id, let result):
                guard state.withdrawOperationRequestID == id, state.isWithdrawing else { return .none }
                state.isWithdrawing = false
                state.withdrawOutcome = result
                if result != .unknown { state.withdrawCommandIDs[id] = nil }
                return .none
            case .appeared, .disappeared: return .none
            case .hiddenInboxChanged(let ids):
                guard state.hiddenInboxIDs != ids else { return .none }
                state.hiddenInboxIDs = ids
                state.presentation = ServiceTaskPresentation(workspace: state.workspace, hiddenIDs: ids)
                if let detail = state.detail {
                    state.detailContext = ServiceTaskContext(presentation: state.presentation, taskID: detail.taskID)
                }
                return .none
            case .workspaceChanged(let workspace):
                state.workspace = workspace
                state.presentation = ServiceTaskPresentation(workspace: workspace, hiddenIDs: state.hiddenInboxIDs)
                state.taskBoard = state.presentation.board
                state.openTrees = state.taskBoard.openTrees
                state.doneTrees = state.taskBoard.doneTrees
                state.archivedTrees = state.taskBoard.archivedTrees
                let pathUpdates: Effect<Action> = .merge(state.taskPath.ids.map { id in
                    let taskID = state.taskPath[id: id]!.taskID
                    return .send(.taskPath(.element(id: id, action: .taskChanged(Self.detail(taskID, workspace), workspace))))
                })
                let projection = TaskBoardProjection(board: state.taskBoard, workspace: workspace, hasSnapshot: workspace != nil)
                if let detail = state.detail {
                    state.detailContext = ServiceTaskContext(presentation: state.presentation, taskID: detail.taskID)
                    return .merge(pathUpdates, .send(.board(.boardChanged(projection))),
                                  .send(.detail(.presented(.taskChanged(Self.detail(detail.taskID, workspace), workspace)))))
                }
                return .merge(pathUpdates, .send(.board(.boardChanged(projection))))
            case .pushTask(let id):
                guard let detail = Self.detail(id, state.workspace) else { return .none }
                state.taskPath.append(TaskDetailFeature.State(taskID: id))
                guard let pathID = state.taskPath.ids.last else { return .none }
                return .send(.taskPath(.element(id: pathID, action: .taskChanged(detail, state.workspace))))
            case .openTask(let id):
                state.detailContext = ServiceTaskContext(presentation: state.presentation, taskID: id)
                state.detail = TaskDetailFeature.State(taskID: id)
                return .send(.detail(.presented(.taskChanged(Self.detail(id, state.workspace), state.workspace))))
            case .showSchedules(let show): state.showingSchedules = show; return .none
            case .deleteSchedule(let id): state.deletingScheduleID = id; return .none
            case .confirmDeleteSchedule:
                guard let id = state.deletingScheduleID else { return .none }
                state.deletingScheduleID = nil
                return .send(.schedules(.deleteConfirmed(id)))
            case .focusInbox(let id): state.focusedInboxID = id; return .none
            case .inboxFocusHandled: state.focusedInboxID = nil; return .none
            case .board, .schedules, .detail, .taskPath: return .none
            }
        }.ifLet(\.$detail, action: \.detail) { TaskDetailFeature() }
            .forEach(\.taskPath, action: \.taskPath) { TaskDetailFeature() }
    }

    static func detail(_ id: DomainV2.TaskId, _ workspace: DomainWorkspace?) -> DomainTaskDetail? {
        workspace.flatMap { DomainTaskDetail(id, in: $0) }
    }
}
