import AlondraDomain
import AlondraShared
import ComposableArchitecture
import Foundation

@Reducer
public struct TaskBoardFeature {
    @ObservableState
    public struct State: Equatable {
        public var openRequestIDs: [String] = []
        public var doneRequestIDs: [String] = []
        public var archivedRequestIDs: [String] = []
        public var hasSnapshot = false
        public var canArchive = false
        public var isArchiving = false
        public var showDone = false
        public var showArchived = false
        public var selectedTaskID: String?
        public var archiveOutcome: TaskMutationOutcome?
        public var presentationEpoch: UInt64 = 0
        public let archiveID = UUID()

        public init() {}
    }

    public enum Action: Sendable {
        case boardChanged(TaskBoardProjection)
        case identityChanged
        case selectTask(String)
        case showDone(Bool)
        case showArchived(Bool)
        case archiveTapped
        case archiveFinished(TaskMutationOutcome, UInt64)
        /// Sent only by the scoped journal owner after reconciling this archive command.
        case commandResolved(UUID, rejection: String?)
        case commandReconciled(UUID)
    }

    @Dependency(\.taskOperations) var operations
    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .boardChanged(let board):
                if state.archiveOutcome == .confirmed, state.doneRequestIDs != board.doneRequestIDs { state.archiveOutcome = nil }
                state.hasSnapshot = board.hasSnapshot
                state.openRequestIDs = board.openRequestIDs
                state.doneRequestIDs = board.doneRequestIDs
                state.archivedRequestIDs = board.archivedRequestIDs
                state.canArchive = board.canArchive
                if let id = state.selectedTaskID, !board.taskIDs.contains(id) {
                    state.selectedTaskID = nil
                }
                return .none
            case .identityChanged:
                state.presentationEpoch &+= 1
                state.openRequestIDs = []
                state.doneRequestIDs = []
                state.archivedRequestIDs = []
                state.selectedTaskID = nil
                state.hasSnapshot = false
                state.canArchive = false
                state.isArchiving = false
                state.archiveOutcome = nil
                return .cancel(id: state.archiveID)
            case .selectTask(let id):
                state.selectedTaskID = id
                return .none
            case .showDone(let show):
                state.showDone = show
                return .none
            case .showArchived(let show):
                state.showArchived = show
                return .none
            case .archiveTapped:
                guard state.canArchive, !state.isArchiving, state.archiveOutcome?.uncertainID == nil, state.archiveOutcome != .confirmed else { return .none }
                state.isArchiving = true
                state.archiveOutcome = nil
                let epoch = state.presentationEpoch
                return .run { send in
                    await send(.archiveFinished(await operations.archiveCompleted(), epoch))
                }
                .cancellable(id: state.archiveID)
            case .archiveFinished(let outcome, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.isArchiving = false
                state.archiveOutcome = outcome
                return .none
            case .commandResolved(let id, let rejection):
                guard state.archiveOutcome?.uncertainID == id else { return .none }
                state.archiveOutcome = rejection.map(TaskMutationOutcome.rejected) ?? .confirmed
                return .none
            case .commandReconciled(let id):
                if state.archiveOutcome?.uncertainID == id { state.archiveOutcome = nil }
                return .none
            }
        }
    }
}

/// The sync owner builds this value from a TaskBoard. Presentation never recomputes server state.
public struct TaskBoardProjection: Sendable {
    public let openRequestIDs: [String]
    public let doneRequestIDs: [String]
    public let archivedRequestIDs: [String]
    public let taskIDs: Set<String>
    public let canArchive: Bool
    public let hasSnapshot: Bool

    public init(board: TaskBoard, canArchive: Bool, hasSnapshot: Bool) {
        openRequestIDs = board.openTrees.map(\.id)
        doneRequestIDs = board.doneTrees.map(\.id)
        archivedRequestIDs = board.archivedTrees.map(\.id)
        taskIDs = Set(board.tasks.keys)
        self.canArchive = canArchive
        self.hasSnapshot = hasSnapshot
    }
}
