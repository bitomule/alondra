import AlondraContracts
import AlondraDomain
import AlondraDomainDependency
import AlondraShared
import ComposableArchitecture
import Foundation

/// Lists requests and tasks from the projection. Archiving the finished ones sends one
/// `archive_task` per task, each offered by that task's own server actions.
@Reducer
public struct TaskBoardFeature {
    @ObservableState
    public struct State: Equatable {
        public var openRequestIDs: [DomainV2.RequestId] = []
        public var doneRequestIDs: [DomainV2.RequestId] = []
        public var archivedRequestIDs: [DomainV2.RequestId] = []
        public var hasSnapshot = false
        public var canArchive = false
        public var isArchiving = false
        public var archiveOutcome: TaskMutationOutcome?
        public var workspace: DomainWorkspace?
        public var finishedTaskIDs: [DomainV2.TaskId] = []
        /// Command IDs of archives whose outcome is unknown; archiving again reuses them.
        public var archiveCommandIDs: [DomainV2.TaskId: String] = [:]
        public var presentationEpoch: UInt64 = 0
        public let archiveID = UUID()
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
        case archiveTapped
        case archiveFinished([DomainV2.TaskId: TaskMutationOutcome], UInt64)
    }

    @Dependency(\.domainWorkspace) var client
    @Dependency(\.uuid) var uuid

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .boardChanged(let board):
                if state.archiveOutcome == .confirmed, state.finishedTaskIDs != board.finishedTaskIDs { state.archiveOutcome = nil }
                state.hasSnapshot = board.hasSnapshot
                state.workspace = board.workspace
                state.finishedTaskIDs = board.finishedTaskIDs
                state.canArchive = !board.finishedTaskIDs.isEmpty
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
                state.presentationEpoch &+= 1
                state.workspace = nil
                state.finishedTaskIDs = []
                state.canArchive = false
                state.isArchiving = false
                state.archiveOutcome = nil
                state.archiveCommandIDs = [:]
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
                guard state.canArchive, !state.isArchiving, state.archiveOutcome != .confirmed,
                      let workspace = state.workspace else { return .none }
                state.isArchiving = true
                state.archiveOutcome = nil
                var commands: [(DomainV2.TaskId, String)] = []
                for id in state.finishedTaskIDs {
                    let command = state.archiveCommandIDs[id] ?? uuid().uuidString.lowercased()
                    state.archiveCommandIDs[id] = command
                    commands.append((id, command))
                }
                let epoch = state.presentationEpoch
                return .run { [commands] send in
                    var outcomes: [DomainV2.TaskId: TaskMutationOutcome] = [:]
                    for (id, command) in commands {
                        let actions = (try? await client.taskActions(id, workspace.epoch)) ?? []
                        outcomes[id] = TaskMutationOutcome(await client.perform(.archiveTask(id, archived: true), workspace, actions, command))
                    }
                    await send(.archiveFinished(outcomes, epoch))
                }
                .cancellable(id: state.archiveID)
            case .archiveFinished(let outcomes, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.isArchiving = false
                for (id, outcome) in outcomes where outcome != .unknown { state.archiveCommandIDs[id] = nil }
                let values = Array(outcomes.values)
                if let rejected = values.first(where: { if case .rejected = $0 { true } else { false } }) {
                    state.archiveOutcome = rejected
                } else if values.contains(.unknown) {
                    state.archiveOutcome = .unknown
                } else {
                    state.archiveOutcome = values.isEmpty ? nil : .confirmed
                }
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
    /// Done or cancelled tasks that are not archived yet.
    public let finishedTaskIDs: [DomainV2.TaskId]
    public let workspace: DomainWorkspace?
    public let hasSnapshot: Bool

    public init(board: TaskBoard, workspace: DomainWorkspace?, hasSnapshot: Bool) {
        finishedTaskIDs = board.activeTrees.flatMap(\.tasks).filter { $0.state.isFinished }.map(\.id)
        self.workspace = workspace
        openRequestIDs = board.openTrees.map(\.id)
        doneRequestIDs = board.doneTrees.map(\.id)
        archivedRequestIDs = board.archivedTrees.map(\.id)
        taskIDs = Set(board.tasks.keys)
        self.hasSnapshot = hasSnapshot
    }
}
