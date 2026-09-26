import AlondraShared
import ComposableArchitecture
import Foundation

@Reducer
public struct TaskDetailFeature {
    @ObservableState
    public struct State: Equatable {
        public let taskID: String
        public var task: WorkTask?
        public var detail: TaskInFull?
        public var isActing = false
        public var confirmation: Confirmation?
        public var mutationOutcome: TaskMutationOutcome?
        public var successfulActionCount = 0
        public var presentationEpoch: UInt64 = 0
        public let effectID = UUID()
        public let mutationID = UUID()

        public init(taskID: String) { self.taskID = taskID }

        public static func == (lhs: Self, rhs: Self) -> Bool {
            lhs.taskID == rhs.taskID && lhs.task == rhs.task &&
                lhs.detail?.task == rhs.detail?.task &&
                lhs.detail?.documents?.map(\.id) == rhs.detail?.documents?.map(\.id) &&
                lhs.isActing == rhs.isActing && lhs.confirmation == rhs.confirmation &&
                lhs.mutationOutcome == rhs.mutationOutcome &&
                lhs.successfulActionCount == rhs.successfulActionCount &&
                lhs.presentationEpoch == rhs.presentationEpoch && lhs.effectID == rhs.effectID &&
                lhs.mutationID == rhs.mutationID
        }
    }

    public enum Confirmation: Equatable, Sendable { case cancel }
    public enum Mutation: Equatable, Sendable { case cancel, reopen }

    public enum Action: Sendable {
        case taskChanged(WorkTask?)
        case detailLoaded(TaskInFull?, Int64, UInt64)
        case cancelTapped
        case cancelConfirmationDismissed
        case cancelConfirmed
        case reopenTapped
        case mutationFinished(TaskMutationOutcome, Mutation, UInt64)
        /// Sent only by the scoped journal owner after it reconciles this exact command ID.
        case commandResolved(UUID, rejection: String?)
        case commandReconciled(UUID)
        case identityChanged
        case disappeared
    }

    @Dependency(\.taskOperations) var operations
    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .taskChanged(let task):
                guard task?.id == state.taskID || task == nil else { return .none }
                let previous = state.task?.historyKey
                if state.mutationOutcome == .confirmed, task?.historyKey != previous { state.mutationOutcome = nil }
                state.task = task
                if task == nil {
                    state.detail = nil
                    state.confirmation = nil
                    return .cancel(id: state.effectID)
                }
                guard let task, task.historyKey != previous else { return .none }
                state.detail = nil
                // A newer history entry can be unrelated to this command. The journal resolves it.
                let epoch = state.presentationEpoch
                return .run { send in
                    await send(.detailLoaded(await operations.details(task), task.historyKey, epoch))
                }
                .cancellable(id: state.effectID, cancelInFlight: true)
            case .detailLoaded(let loaded, let key, let epoch):
                guard epoch == state.presentationEpoch, state.task?.historyKey == key else { return .none }
                if let loaded, loaded.task.id == state.taskID { state.detail = loaded }
                return .none
            case .cancelTapped:
                guard state.task?.state.isFinished == false, !state.isActing,
                    state.mutationOutcome?.uncertainID == nil, state.mutationOutcome != .confirmed else { return .none }
                state.confirmation = .cancel
                return .none
            case .cancelConfirmationDismissed:
                state.confirmation = nil
                return .none
            case .cancelConfirmed:
                guard state.confirmation == .cancel, let task = state.task,
                    !task.state.isFinished, !state.isActing,
                    state.mutationOutcome?.uncertainID == nil, state.mutationOutcome != .confirmed else { return .none }
                state.confirmation = nil
                state.isActing = true
                state.mutationOutcome = nil
                let epoch = state.presentationEpoch
                return .run { send in
                    await send(.mutationFinished(await operations.cancel(task), .cancel, epoch))
                }
                .cancellable(id: state.mutationID)
            case .reopenTapped:
                guard let task = state.task, task.state.isFinished, !state.isActing,
                    state.mutationOutcome?.uncertainID == nil, state.mutationOutcome != .confirmed else { return .none }
                state.isActing = true
                state.mutationOutcome = nil
                let epoch = state.presentationEpoch
                return .run { send in
                    await send(.mutationFinished(await operations.reopen(task), .reopen, epoch))
                }
                .cancellable(id: state.mutationID)
            case .mutationFinished(let outcome, _, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.isActing = false
                state.mutationOutcome = outcome
                if outcome == .confirmed { state.successfulActionCount += 1 }
                // The next sync projection owns the task state. A successful POST does not.
                return .none
            case .commandResolved(let id, let rejection):
                guard state.mutationOutcome?.uncertainID == id else { return .none }
                state.mutationOutcome = rejection.map(TaskMutationOutcome.rejected) ?? .confirmed
                return .none
            case .commandReconciled(let id):
                if state.mutationOutcome?.uncertainID == id { state.mutationOutcome = nil }
                return .none
            case .identityChanged:
                state.presentationEpoch &+= 1
                state.task = nil
                state.detail = nil
                state.confirmation = nil
                state.isActing = false
                state.mutationOutcome = nil
                return .merge(.cancel(id: state.effectID), .cancel(id: state.mutationID))
            case .disappeared:
                return .cancel(id: state.effectID)
            }
        }
    }
}
