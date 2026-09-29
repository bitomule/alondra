import AlondraDomain
import AlondraShared
import ComposableArchitecture
import Foundation

public enum ScheduleCommandTarget: Hashable, Sendable {
    case new
    case schedule(String)
}

public struct ScheduleAttempt: Equatable, Sendable {
    public enum Operation: Equatable, Sendable { case save(ScheduledTaskDraft), run, delete }
    public let id: String
    public let operation: Operation
    public init(id: String, operation: Operation) { self.id = id; self.operation = operation }
}

@Reducer
public struct ScheduleFeature {
    @ObservableState
    public struct State: Equatable {
        public var tasks: [ScheduledTask] = []
        public var selectedID: String?
        public var inspectorVisible = false
        public var editingID: String?
        public var draft: ScheduledTaskDraft?
        public var optionSpecs: [OptionSpec] = []
        public var projects: [String] = []
        public var isLoading = false
        public var isSaving = false
        public var busyIDs: Set<String> = []
        /// One command at a time per schedule, across save/run/enable/delete.
        public var inFlightTargets: Set<ScheduleCommandTarget> = []
        public var submittedDrafts: [UInt64: ScheduledTaskDraft] = [:]
        /// The attempt ID of a command whose outcome is unknown, with the draft it sent. Sending
        /// the same draft again reuses it, so the Mac returns the first result.
        public var attempts: [ScheduleCommandTarget: ScheduleAttempt] = [:]
        public var lastMutationOutcome: TaskMutationOutcome?
        public var loadError: String?
        public var catalogError: String?
        public var mutationError: String?
        public var error: String? { mutationError ?? catalogError ?? loadError }
        public var optionsLoading = false
        public var optionsReady = false
        public var presentationEpoch: UInt64 = 0
        public var editorRevision: UInt64 = 0
        public var optionsRequest: UInt64 = 0
        public let loadID = UUID()
        public let optionsID = UUID()
        public let mutationID = UUID()

        public var canSaveDraft: Bool {
            guard let draft, ScheduleDraftRules.canSave(draft), optionsReady, !optionsLoading, !isSaving else { return false }
            let target = editingID.map(ScheduleCommandTarget.schedule) ?? .new
            return !inFlightTargets.contains(target)
        }

        public init() {}
    }

    public enum Action: Sendable {
        case appeared
        case refresh
        case reconcile
        case loaded(Result<[ScheduledTask], ScheduleFailure>, UInt64)
        case createTapped
        case editTapped(String)
        case draftChanged(ScheduledTaskDraft)
        case optionsLoaded(Result<([String], [OptionSpec]), ScheduleFailure>, String?, UInt64, UInt64)
        case editorDismissed
        case saveTapped
        case saved(TaskMutationOutcome, ScheduleCommandTarget, UInt64, UInt64)
        case runTapped(String)
        case runFinished(String, TaskMutationOutcome, UInt64)
        case enabledChanged(String, Bool)
        case enabledSaved(String, TaskMutationOutcome, UInt64)
        case deleteConfirmed(String)
        case deleted(String, TaskMutationOutcome, UInt64)
        case selected(String?)
        case inspectorPresented(Bool)
        case identityChanged
    }

    @Dependency(\.scheduleOperations) var operations
    @Dependency(\.uuid) var uuid
    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .appeared, .refresh, .reconcile:
                if case .refresh = action { state.mutationError = nil }
                state.isLoading = true
                let epoch = state.presentationEpoch
                return .run { send in
                    await send(.loaded(await capture { try await operations.list() }, epoch))
                }
                .cancellable(id: state.loadID, cancelInFlight: true)
            case .loaded(let result, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.isLoading = false
                switch result {
                case .success(let tasks):
                    state.tasks = tasks
                    state.loadError = nil
                    if let selectedID = state.selectedID, !tasks.contains(where: { $0.id == selectedID }) {
                        state.selectedID = nil
                        state.inspectorVisible = false
                    }
                case .failure(let error): state.loadError = error.message
                }
                return .none
            case .createTapped:
                state.editorRevision &+= 1
                state.editingID = nil
                state.draft = ScheduledTaskDraft()
                state.mutationError = nil
                state.optionSpecs = []
                state.projects = []
                state.optionsReady = false
                return loadOptions(&state)
            case .editTapped(let id):
                guard let task = state.tasks.first(where: { $0.id == id }) else { return .none }
                state.editorRevision &+= 1
                state.editingID = id
                state.draft = ScheduledTaskDraft(task)
                state.mutationError = nil
                state.optionSpecs = []
                state.projects = []
                state.optionsReady = false
                return loadOptions(&state)
            case .draftChanged(let draft):
                guard let old = state.draft else { return .none }
                state.draft = draft
                guard old.project != draft.project else { return .none }
                state.optionSpecs = []
                state.optionsReady = false
                state.draft?.options = [:]
                return loadOptions(&state)
            case .optionsLoaded(let result, let project, let request, let epoch):
                guard epoch == state.presentationEpoch, request == state.optionsRequest,
                    state.draft?.project == project else { return .none }
                state.optionsLoading = false
                switch result {
                case .success(let values):
                    state.optionsReady = true
                    state.catalogError = nil
                    state.projects = values.0
                    state.optionSpecs = values.1
                    let chosen = state.draft?.options ?? [:]
                    state.draft?.options = ScheduleDraftRules.validOverrides(chosen, in: values.1)
                case .failure(let error): state.catalogError = error.message
                }
                return .none
            case .editorDismissed:
                state.editorRevision &+= 1
                state.draft = nil
                state.editingID = nil
                state.optionSpecs = []
                state.optionsLoading = false
                state.optionsReady = false
                state.isSaving = false
                state.optionsRequest &+= 1
                return .cancel(id: state.optionsID)
            case .saveTapped:
                guard state.canSaveDraft, let draft = state.draft else { return .none }
                let target: ScheduleCommandTarget = state.editingID.map { .schedule($0) } ?? .new
                guard state.inFlightTargets.insert(target).inserted else { return .none }
                var submitted = draft
                submitted.options = ScheduleDraftRules.validOverrides(draft.options, in: state.optionSpecs)
                let payload = submitted
                state.isSaving = true
                state.mutationError = nil
                let id = state.editingID
                let epoch = state.presentationEpoch
                let revision = state.editorRevision
                state.submittedDrafts[revision] = draft
                let attempt = attempt(for: target, sending: .save(payload), &state)
                return .run { send in
                    await send(.saved(await operations.save(payload, id, attempt), target, epoch, revision))
                }
                .cancellable(id: state.mutationID)
            case .saved(let outcome, let target, let epoch, let revision):
                guard epoch == state.presentationEpoch else { return .none }
                state.inFlightTargets.remove(target)
                record(outcome, for: target, in: &state)
                guard revision == state.editorRevision else {
                    return .send(.reconcile)
                }
                state.isSaving = false
                switch outcome {
                case .confirmed:
                    if state.draft == state.submittedDrafts[revision] {
                        state.draft = nil
                        state.editingID = nil
                    }
                    state.submittedDrafts[revision] = nil
                    return .send(.reconcile)
                case .unknown: return .send(.reconcile)
                case .rejected, .obsolete: return .none
                }
            case .runTapped(let id):
                guard state.tasks.contains(where: { $0.id == id }),
                    state.inFlightTargets.insert(.schedule(id)).inserted else { return .none }
                state.busyIDs.insert(id)
                state.mutationError = nil
                let epoch = state.presentationEpoch
                let attempt = attempt(for: .schedule(id), sending: .run, &state)
                return .run { send in
                    await send(.runFinished(id, await operations.run(id, attempt), epoch))
                }
                .cancellable(id: state.mutationID)
            case .runFinished(let id, let outcome, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.busyIDs.remove(id)
                state.inFlightTargets.remove(.schedule(id))
                record(outcome, for: .schedule(id), in: &state)
                return .send(.reconcile)
            case .enabledChanged(let id, let enabled):
                guard let task = state.tasks.first(where: { $0.id == id }),
                    state.inFlightTargets.insert(.schedule(id)).inserted else { return .none }
                state.busyIDs.insert(id)
                state.mutationError = nil
                var draft = ScheduledTaskDraft(task)
                draft.enabled = enabled
                let submitted = draft
                let epoch = state.presentationEpoch
                let attempt = attempt(for: .schedule(id), sending: .save(submitted), &state)
                return .run { send in
                    await send(.enabledSaved(id, await operations.save(submitted, id, attempt), epoch))
                }
                .cancellable(id: state.mutationID)
            case .enabledSaved(let id, let outcome, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.busyIDs.remove(id)
                state.inFlightTargets.remove(.schedule(id))
                record(outcome, for: .schedule(id), in: &state)
                return .send(.reconcile)
            case .deleteConfirmed(let id):
                guard state.tasks.contains(where: { $0.id == id }),
                    state.inFlightTargets.insert(.schedule(id)).inserted else { return .none }
                state.busyIDs.insert(id)
                state.mutationError = nil
                let epoch = state.presentationEpoch
                let attempt = attempt(for: .schedule(id), sending: .delete, &state)
                return .run { send in
                    await send(.deleted(id, await operations.delete(id, attempt), epoch))
                }
                .cancellable(id: state.mutationID)
            case .deleted(let id, let outcome, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.busyIDs.remove(id)
                state.inFlightTargets.remove(.schedule(id))
                record(outcome, for: .schedule(id), in: &state)
                return .send(.reconcile)
            case .selected(let id):
                guard id == nil || state.tasks.contains(where: { $0.id == id }) else { return .none }
                guard state.selectedID != id else { return .none }
                state.selectedID = id
                state.inspectorVisible = id != nil
                return .none
            case .inspectorPresented(let presented):
                state.inspectorVisible = presented && state.selectedID != nil
                return .none
            case .identityChanged:
                state.presentationEpoch &+= 1
                state.editorRevision &+= 1
                state.optionsRequest &+= 1
                state.tasks = []
                state.selectedID = nil
                state.inspectorVisible = false
                state.editingID = nil
                state.draft = nil
                state.optionSpecs = []
                state.projects = []
                state.busyIDs = []
                state.inFlightTargets = []
                state.attempts = [:]
                state.lastMutationOutcome = nil
                state.isLoading = false
                state.isSaving = false
                state.loadError = nil
                state.catalogError = nil
                state.mutationError = nil
                state.optionsLoading = false
                state.optionsReady = false
                return .merge(.cancel(id: state.loadID), .cancel(id: state.optionsID), .cancel(id: state.mutationID))
            }
        }
    }

    private func loadOptions(_ state: inout State) -> Effect<Action> {
        guard let project = state.draft?.project else {
            return requestOptions(&state, project: nil)
        }
        return requestOptions(&state, project: project)
    }

    private func record(_ outcome: TaskMutationOutcome, for target: ScheduleCommandTarget, in state: inout State) {
        state.lastMutationOutcome = outcome
        if outcome != .unknown { state.attempts[target] = nil }
        switch outcome {
        case .rejected(let message): state.mutationError = message
        case .unknown: state.mutationError = String(localized: "We couldn’t confirm the result. Sending it again won’t apply it twice.")
        default: break
        }
    }

    /// The same operation on the same target after an unknown outcome reuses its attempt ID;
    /// anything else is a new attempt.
    private func attempt(for target: ScheduleCommandTarget, sending operation: ScheduleAttempt.Operation,
                         _ state: inout State) -> String {
        if let pending = state.attempts[target], pending.operation == operation { return pending.id }
        let attempt = ScheduleAttempt(id: uuid().uuidString, operation: operation)
        state.attempts[target] = attempt
        return attempt.id
    }

    private func requestOptions(_ state: inout State, project: String?) -> Effect<Action> {
        state.optionsRequest &+= 1
        state.optionsLoading = true
        state.optionsReady = false
        state.catalogError = nil
        let request = state.optionsRequest
        let epoch = state.presentationEpoch
        return .run { send in
            let result: Result<([String], [OptionSpec]), ScheduleFailure>
            do {
                async let projects = operations.projects()
                async let options = operations.options(project)
                result = .success(try await (projects, options))
            } catch { result = .failure(ScheduleFailure(error)) }
            await send(.optionsLoaded(result, project, request, epoch))
        }
        .cancellable(id: state.optionsID, cancelInFlight: true)
    }
}

public struct ScheduleFailure: Error, Equatable, Sendable {
    public let message: String
    public init(_ error: Error) { message = error.localizedDescription }
}

private func capture<Value: Sendable>(_ operation: () async throws -> Value) async -> Result<Value, ScheduleFailure> {
    do { return .success(try await operation()) }
    catch { return .failure(ScheduleFailure(error)) }
}
