import AlondraShared
import AlondraTasksFeature
import ComposableArchitecture
import Foundation

struct ServiceTasksClient: Sendable {
    var observe: @Sendable () async -> AsyncStream<Overview?>
}
private enum ServiceTasksClientKey: DependencyKey {
    static let liveValue = ServiceTasksClient(observe: { preconditionFailure("Install service tasks client") })
    static let testValue = liveValue
}
extension DependencyValues {
    var serviceTasksClient: ServiceTasksClient {
        get { self[ServiceTasksClientKey.self] }
        set { self[ServiceTasksClientKey.self] = newValue }
    }
}

@Reducer
struct ServiceTasksFeature {
    @ObservableState
    struct State {
        var board = TaskBoardFeature.State()
        var schedules = ScheduleFeature.State()
        var overview: Overview?
        var taskBoard = TaskBoard.empty
        var openTrees: [RequestTree] = []
        var doneTrees: [RequestTree] = []
        var archivedTrees: [RequestTree] = []
        @Presents var detail: TaskDetailFeature.State?
        var detailContext = ServiceTaskContext(board: .empty, overview: nil, taskID: "")
        var showingSchedules = false
        var deletingScheduleID: String?
        var focusedInboxID: String?
    }
    enum Action {
        case appeared, disappeared
        case snapshot(Overview?)
        case board(TaskBoardFeature.Action)
        case schedules(ScheduleFeature.Action)
        case detail(PresentationAction<TaskDetailFeature.Action>)
        case openTask(String)
        case showSchedules(Bool)
        case deleteSchedule(String?)
        case confirmDeleteSchedule
        case focusInbox(String)
    }
    @Dependency(\.serviceTasksClient) var client
    private enum CancelID { case observation }
    var body: some ReducerOf<Self> {
        Scope(state: \.board, action: \.board) { TaskBoardFeature() }
        Scope(state: \.schedules, action: \.schedules) { ScheduleFeature() }
        Reduce { state, action in
            switch action {
            case .appeared:
                return .run { send in
                    for await overview in await client.observe() { await send(.snapshot(overview)) }
                }.cancellable(id: CancelID.observation, cancelInFlight: true)
            case .disappeared: return .cancel(id: CancelID.observation)
            case .snapshot(let overview):
                state.overview = overview
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
            case .board, .schedules, .detail: return .none
            }
        }.ifLet(\.$detail, action: \.detail) { TaskDetailFeature() }
    }
}
