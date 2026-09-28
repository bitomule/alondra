import AlondraContracts
import AlondraDomain
import AlondraDomainDependency
import AlondraAttachmentsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

/// One Task resolved by explicit IDs. Cancel, reopen and archive are offered only when the
/// server's task actions say so, and each sends exactly that command.
@Reducer
public struct TaskDetailFeature {
    @ObservableState
    public struct State: Equatable {
        public var downloads = AttachmentDownloadFeature.State()
        public let taskID: DomainV2.TaskId
        public var detail: DomainTaskDetail?
        public var workspace: DomainWorkspace?
        public var actions: [DomainV2.AvailableAction] = []
        public var actionsRevision: DomainV2.Revision?
        public var isActing = false
        public var confirmation: Confirmation?
        public var mutationOutcome: TaskMutationOutcome?
        /// The command ID of an attempt whose outcome is unknown; the same action again reuses it.
        public var commandID: String?
        public var commandKind: DomainV2.ActionKind?
        public var successfulActionCount = 0
        public var presentationEpoch: UInt64 = 0
        public let effectID = UUID()
        public let mutationID = UUID()

        public init(taskID: DomainV2.TaskId) { self.taskID = taskID }

        public var task: DomainV2.Task? { detail?.task }

        public func action(_ kind: DomainV2.ActionKind) -> DomainV2.AvailableAction? {
            actions.first { $0.actionKind == kind }
        }

        public var canAct: Bool {
            !isActing && mutationOutcome != .confirmed
        }

        public func canPerform(_ kind: DomainV2.ActionKind) -> Bool {
            canAct && action(kind)?.enabled == true
        }
    }

    public enum Confirmation: Equatable, Sendable { case cancel }

    public enum Action: Sendable {
        case downloads(AttachmentDownloadFeature.Action)
        case taskChanged(DomainTaskDetail?, DomainWorkspace?)
        case actionsLoaded([DomainV2.AvailableAction], DomainV2.Revision, UInt64)
        case cancelTapped
        case cancelConfirmationDismissed
        case cancelConfirmed
        case reopenTapped
        case archiveTapped(Bool)
        case mutationFinished(TaskMutationOutcome, UInt64)
        case identityChanged
        case disappeared
    }

    @Dependency(\.domainWorkspace) var client
    @Dependency(\.uuid) var uuid
    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.downloads, action: \.downloads) { AttachmentDownloadFeature() }
        Reduce { state, action in
            switch action {
            case .downloads: return .none
            case let .taskChanged(detail, workspace):
                guard detail?.task.id == state.taskID || detail == nil else { return .none }
                let previous = state.detail?.task.revision
                if state.mutationOutcome == .confirmed, detail?.task.revision != previous { state.mutationOutcome = nil }
                state.detail = detail
                state.workspace = workspace
                guard let detail, let workspace else {
                    state.actions = []
                    state.actionsRevision = nil
                    state.confirmation = nil
                    return .cancel(id: state.effectID)
                }
                guard state.actionsRevision != detail.task.revision else { return .none }
                let id = state.taskID
                let revision = detail.task.revision
                let epoch = state.presentationEpoch
                return .run { send in
                    let actions = (try? await client.taskActions(id, workspace.epoch)) ?? []
                    await send(.actionsLoaded(actions, revision, epoch))
                }
                .cancellable(id: state.effectID, cancelInFlight: true)
            case let .actionsLoaded(actions, revision, epoch):
                guard epoch == state.presentationEpoch, state.detail?.task.revision == revision else { return .none }
                state.actions = actions
                state.actionsRevision = revision
                return .none
            case .cancelTapped:
                guard state.canPerform(.cancelTask) else { return .none }
                state.confirmation = .cancel
                return .none
            case .cancelConfirmationDismissed:
                state.confirmation = nil
                return .none
            case .cancelConfirmed:
                guard state.confirmation == .cancel else { return .none }
                state.confirmation = nil
                return perform(.cancelTask(state.taskID), kind: .cancelTask, &state)
            case .reopenTapped:
                return perform(.reopenTask(state.taskID), kind: .reopenTask, &state)
            case .archiveTapped(let archived):
                return perform(.archiveTask(state.taskID, archived: archived), kind: archived ? .archiveTask : .unarchiveTask, &state)
            case .mutationFinished(let outcome, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.isActing = false
                state.mutationOutcome = outcome
                if outcome != .unknown { state.commandID = nil }
                if outcome == .confirmed { state.successfulActionCount += 1 }
                // The next projection owns the task state. A recorded command does not.
                return .none
            case .identityChanged:
                state.presentationEpoch &+= 1
                state.detail = nil
                state.workspace = nil
                state.actions = []
                state.actionsRevision = nil
                state.confirmation = nil
                state.isActing = false
                state.mutationOutcome = nil
                state.commandID = nil
                return .merge(.cancel(id: state.effectID), .cancel(id: state.mutationID))
            case .disappeared:
                return .cancel(id: state.effectID)
            }
        }
    }

    private func perform(_ intent: DomainIntent, kind: DomainV2.ActionKind, _ state: inout State) -> Effect<Action> {
        guard state.canPerform(kind), let workspace = state.workspace else { return .none }
        state.isActing = true
        state.mutationOutcome = nil
        if state.commandKind != kind { state.commandID = nil }
        let commandID = state.commandID ?? uuid().uuidString.lowercased()
        state.commandID = commandID
        state.commandKind = kind
        let actions = state.actions
        let epoch = state.presentationEpoch
        return .run { send in
            let result = await client.perform(intent, workspace, actions, commandID)
            await send(.mutationFinished(TaskMutationOutcome(result), epoch))
        }
        .cancellable(id: state.mutationID)
    }
}

extension TaskMutationOutcome {
    public init(_ result: DomainCommandResult) {
        switch result {
        case .recorded: self = .confirmed
        case .rejected(let rejection): self = .rejected(rejection.localizedMessage)
        case .uncertain: self = .unknown
        case .obsolete: self = .obsolete
        }
    }
}
