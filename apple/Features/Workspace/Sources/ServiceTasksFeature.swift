import AlondraShared
import AlondraTasksFeature
import ComposableArchitecture
import Foundation

public struct ServiceTasksClient: Sendable {
    public init(refresh: @escaping @Sendable () async -> Void = { preconditionFailure("Install service refresh") }, observe: @escaping @Sendable () async -> AsyncStream<Overview?>) { self.observe = observe; self.refresh = refresh }
    public var refresh: @Sendable () async -> Void
    public var observe: @Sendable () async -> AsyncStream<Overview?>
}
private enum ServiceTasksClientKey: DependencyKey {
    static let liveValue = ServiceTasksClient(observe: { preconditionFailure("Install service tasks client") })
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
        public var schedules = ScheduleFeature.State()
        public var presentation = ServiceTaskPresentation(overview: nil)
        public var withdrawingRequestID: String?
        public var isWithdrawing = false
        public var withdrawOutcome: TaskMutationOutcome?
        public var withdrawOperationRequestID: String?
        public var overview: Overview?
        public var taskBoard = TaskBoard.empty
        public var openTrees: [RequestTree] = []
        public var doneTrees: [RequestTree] = []
        public var archivedTrees: [RequestTree] = []
        @Presents public var detail: TaskDetailFeature.State?
        public var detailContext = ServiceTaskContext(board: .empty, overview: nil, taskID: "")
        public var showingSchedules = false
        public var deletingScheduleID: String?
        public var focusedInboxID: String?
    }
    public enum Action {
        case appeared, disappeared, refresh
        case withdrawRequested(String), withdrawCancelled, withdrawConfirmed
        case withdrawFinished(String, TaskMutationOutcome)
        case withdrawalResolved(UUID, rejection: String?)
        case snapshot(Overview?)
        case board(TaskBoardFeature.Action)
        case schedules(ScheduleFeature.Action)
        case detail(PresentationAction<TaskDetailFeature.Action>)
        case openTask(String)
        case showSchedules(Bool)
        case deleteSchedule(String?)
        case confirmDeleteSchedule
        case focusInbox(String)
        case inboxFocusHandled
    }
    @Dependency(\.taskOperations) var taskOperations
    @Dependency(\.serviceTasksClient) var client
    private enum CancelID { case observation }
    public var body: some ReducerOf<Self> {
        Scope(state: \.board, action: \.board) { TaskBoardFeature() }
        Scope(state: \.schedules, action: \.schedules) { ScheduleFeature() }
        Reduce { state, action in
            switch action {
            case .refresh: return .run { _ in await client.refresh() }
            case .withdrawRequested(let id):
                guard !state.isWithdrawing, state.withdrawOutcome?.uncertainID == nil,
                      state.taskBoard.request(id)?.canWithdraw == true else { return .none }
                state.withdrawingRequestID = id
                return .none
            case .withdrawCancelled: state.withdrawingRequestID = nil; return .none
            case .withdrawConfirmed:
                guard let id = state.withdrawingRequestID, !state.isWithdrawing,
                      state.withdrawOutcome?.uncertainID == nil,
                      let request = state.taskBoard.request(id), request.canWithdraw else { return .none }
                state.withdrawingRequestID = nil
                state.withdrawOperationRequestID = id
                state.isWithdrawing = true
                state.withdrawOutcome = nil
                return .run { send in await send(.withdrawFinished(id, await taskOperations.withdraw(request))) }
            case .withdrawFinished(let id, let result):
                guard state.withdrawOperationRequestID == id, state.isWithdrawing else { return .none }
                state.isWithdrawing = false
                state.withdrawOutcome = result
                return .none
            case .withdrawalResolved(let id, let rejection):
                guard state.withdrawOutcome?.uncertainID == id else { return .none }
                state.withdrawOutcome = rejection.map(TaskMutationOutcome.rejected) ?? .confirmed
                return .none
            case .appeared:
                return .run { send in
                    for await overview in await client.observe() { await send(.snapshot(overview)) }
                }.cancellable(id: CancelID.observation, cancelInFlight: true)
            case .disappeared: return .cancel(id: CancelID.observation)
            case .snapshot(let overview):
                state.overview = overview
                state.presentation = ServiceTaskPresentation(overview: overview)
                state.taskBoard = TaskBoard(requests: overview?.requests ?? [], tasks: overview?.tasks ?? [])
                state.openTrees = state.taskBoard.openTrees
                state.doneTrees = state.taskBoard.doneTrees
                state.archivedTrees = state.taskBoard.archivedTrees
                let projection = TaskBoardProjection(board: state.taskBoard, canArchive: state.taskBoard.hasArchivable, hasSnapshot: overview != nil)
                if let detail = state.detail {
                    state.detailContext = ServiceTaskContext(board: state.taskBoard, overview: overview, taskID: detail.taskID)
                    return .merge(.send(.board(.boardChanged(projection))), .send(.detail(.presented(.taskChanged(state.taskBoard.task(detail.taskID))))))
                }
                return .send(.board(.boardChanged(projection)))
            case .openTask(let id):
                state.detailContext = ServiceTaskContext(board: state.taskBoard, overview: state.overview, taskID: id)
                state.detail = TaskDetailFeature.State(taskID: id)
                return .send(.detail(.presented(.taskChanged(state.taskBoard.task(id)))))
            case .showSchedules(let show): state.showingSchedules = show; return .none
            case .deleteSchedule(let id): state.deletingScheduleID = id; return .none
            case .confirmDeleteSchedule:
                guard let id = state.deletingScheduleID else { return .none }
                state.deletingScheduleID = nil
                return .send(.schedules(.deleteConfirmed(id)))
            case .focusInbox(let id): state.focusedInboxID = id; return .none
            case .inboxFocusHandled: state.focusedInboxID = nil; return .none
            case .board, .schedules, .detail: return .none
            }
        }.ifLet(\.$detail, action: \.detail) { TaskDetailFeature() }
    }
}
