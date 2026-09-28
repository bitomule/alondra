import AlondraContracts
import AlondraDomain
import AlondraShared
import ComposableArchitecture
import Foundation

/// Lists requests and tasks from the projection. Archiving is a per-task server action in the
/// task detail; the board sends no command of its own.
@Reducer
public struct TaskBoardFeature {
    @ObservableState
    public struct State: Equatable {
        public var openRequestIDs: [DomainV2.RequestId] = []
        public var doneRequestIDs: [DomainV2.RequestId] = []
        public var archivedRequestIDs: [DomainV2.RequestId] = []
        public var hasSnapshot = false
        public var showDone = false
        public var showArchived = false
        public var selectedTaskID: DomainV2.TaskId?

        public init() {}
    }

    public enum Action: Sendable {
        case boardChanged(TaskBoardProjection)
        case identityChanged
        case selectTask(DomainV2.TaskId)
        case showDone(Bool)
        case showArchived(Bool)
    }

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .boardChanged(let board):
                state.hasSnapshot = board.hasSnapshot
                state.openRequestIDs = board.openRequestIDs
                state.doneRequestIDs = board.doneRequestIDs
                state.archivedRequestIDs = board.archivedRequestIDs
                if let id = state.selectedTaskID, !board.taskIDs.contains(id) {
                    state.selectedTaskID = nil
                }
                return .none
            case .identityChanged:
                state.openRequestIDs = []
                state.doneRequestIDs = []
                state.archivedRequestIDs = []
                state.selectedTaskID = nil
                state.hasSnapshot = false
                return .none
            case .selectTask(let id):
                state.selectedTaskID = id
                return .none
            case .showDone(let show):
                state.showDone = show
                return .none
            case .showArchived(let show):
                state.showArchived = show
                return .none
            }
        }
    }
}

/// The sync owner builds this value from a TaskBoard. Presentation never recomputes server state.
public struct TaskBoardProjection: Sendable {
    public let openRequestIDs: [DomainV2.RequestId]
    public let doneRequestIDs: [DomainV2.RequestId]
    public let archivedRequestIDs: [DomainV2.RequestId]
    public let taskIDs: Set<DomainV2.TaskId>
    public let hasSnapshot: Bool

    public init(board: TaskBoard, hasSnapshot: Bool) {
        openRequestIDs = board.openTrees.map(\.id)
        doneRequestIDs = board.doneTrees.map(\.id)
        archivedRequestIDs = board.archivedTrees.map(\.id)
        taskIDs = Set(board.tasks.keys)
        self.hasSnapshot = hasSnapshot
    }
}
