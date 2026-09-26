import AlondraDomain
import AlondraShared
import ComposableArchitecture
import Foundation

@Reducer
public struct NewRequestFeature {
    @ObservableState
    public struct State: Equatable {
        public var text = ""
        public var attachments: [NewRequestAttachment] = []
        public var selection = NewWorkSelection()
        public var projects: [String] = []
        public var issues: [IssueSummary] = []
        public var issueQuery = ""
        public var issuesLoaded = false
        public var isLoadingIssues = false
        public var isLoadingOptions = false
        public var catalogIsCurrent = false
        public var isSubmitting = false
        public var canSubmit = false
        public var optionsError: String?
        public var issuesError: String?
        public var submissionError: String?
        public var uncertainCommandID: UUID?
        public var submissionUncertain = false
        public var successfulSubmissionCount = 0
        public var presentationEpoch: UInt64 = 0
        public var catalogGeneration: UInt64 = 0

        public init() {}

        public var visibleIssues: [IssueSummary] { issues.filter { $0.matches(issueQuery) } }
        public var pastedIssue: String? { IssueSummary.pasted(in: issueQuery, besides: issues) }
        public var submitEnabled: Bool {
            canSubmit && !isSubmitting && catalogIsCurrent && !isLoadingOptions && !submissionUncertain
                && (!text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty)
        }
    }

    public enum Action: Sendable {
        case appeared
        case identityChanged
        case availabilityChanged(Bool)
        case textChanged(String)
        case attachmentAdded(NewRequestAttachment)
        case attachmentRemoved(UUID)
        case projectSelected(String?)
        case optionSelected(String, OptionChoice)
        case issueSelected(String?)
        case issueQueryChanged(String)
        case issuesOpened
        case issuesRefreshRequested
        case projectsLoaded([String], UInt64)
        case projectsFailed(String, UInt64)
        case optionsLoaded(OptionsCatalog, UInt64, UInt64)
        case optionsFailed(String, UInt64, UInt64)
        case issuesLoaded(IssueList, UInt64)
        case issuesFailed(String, UInt64)
        case submitTapped
        case submitFinished(CreateRequestResult, UInt64)
        case commandResolved(UUID, rejection: String?)
        case discardUncertainDraft
    }

    @Dependency(\.newWorkOperations) var operations
    public init() {}

    private enum CancelID { case projects, options, issues, submit }

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .appeared:
                let epoch = state.presentationEpoch
                state.catalogGeneration &+= 1
                state.isLoadingOptions = true
                state.catalogIsCurrent = false
                return .merge(
                    .run { send in
                        do { await send(.projectsLoaded(try await operations.projects(), epoch)) }
                        catch { await send(.projectsFailed(error.localizedDescription, epoch)) }
                    }.cancellable(id: CancelID.projects, cancelInFlight: true),
                    loadOptions(project: state.selection.project, epoch: epoch, generation: state.catalogGeneration)
                )
            case .identityChanged:
                let nextEpoch = state.presentationEpoch &+ 1
                state = State()
                state.presentationEpoch = nextEpoch
                return .merge(
                    .cancel(id: CancelID.projects), .cancel(id: CancelID.options),
                    .cancel(id: CancelID.issues), .cancel(id: CancelID.submit)
                )
            case .availabilityChanged(let available):
                state.canSubmit = available
                return .none
            case .textChanged(let text):
                guard !state.isSubmitting, !state.submissionUncertain else { return .none }
                state.text = text
                return .none
            case .attachmentAdded(let attachment):
                guard !state.isSubmitting, !state.submissionUncertain else { return .none }
                guard !state.attachments.contains(where: { $0.id == attachment.id }) else { return .none }
                state.attachments.append(attachment)
                return .none
            case .attachmentRemoved(let id):
                guard !state.isSubmitting, !state.submissionUncertain else { return .none }
                state.attachments.removeAll { $0.id == id }
                return .none
            case .projectSelected(let project):
                guard !state.isSubmitting, !state.submissionUncertain else { return .none }
                guard state.selection.project != project else { return .none }
                state.selection.project = project
                state.catalogGeneration &+= 1
                state.isLoadingOptions = true
                state.catalogIsCurrent = false
                state.optionsError = nil
                return loadOptions(project: project, epoch: state.presentationEpoch, generation: state.catalogGeneration)
            case .optionSelected(let id, let choice):
                guard !state.isSubmitting, !state.submissionUncertain else { return .none }
                guard let spec = state.selection.specs.first(where: { $0.id == id }) else { return .none }
                state.selection.set(spec, to: choice)
                return .none
            case .issueSelected(let issue):
                guard !state.isSubmitting, !state.submissionUncertain else { return .none }
                let previousProject = state.selection.project
                state.selection.select(issue: issue, inferredProject: IssueSummary.project(of: issue, among: state.projects))
                guard state.selection.project != previousProject else { return .none }
                state.catalogGeneration &+= 1
                state.isLoadingOptions = true
                state.catalogIsCurrent = false
                state.optionsError = nil
                return loadOptions(project: state.selection.project, epoch: state.presentationEpoch, generation: state.catalogGeneration)
            case .issueQueryChanged(let query):
                state.issueQuery = query
                return .none
            case .issuesOpened:
                guard !state.issuesLoaded, !state.isLoadingIssues else { return .none }
                return loadIssues(state: &state, refresh: false)
            case .issuesRefreshRequested:
                guard !state.isLoadingIssues else { return .none }
                return loadIssues(state: &state, refresh: true)
            case .projectsLoaded(let projects, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.projects = projects
                if let issue = state.selection.issue,
                   let inferred = IssueSummary.project(of: issue, among: projects),
                   inferred != state.selection.project {
                    state.selection.select(issue: issue, inferredProject: inferred)
                    state.catalogGeneration &+= 1
                    state.isLoadingOptions = true
                    state.catalogIsCurrent = false
                    return loadOptions(project: inferred, epoch: epoch, generation: state.catalogGeneration)
                }
                return .none
            case .projectsFailed(_, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                // A failed project list must not erase a manually selected project.
                return .none
            case .optionsLoaded(let catalog, let epoch, let generation):
                guard epoch == state.presentationEpoch, generation == state.catalogGeneration else { return .none }
                state.selection.apply(catalog)
                state.isLoadingOptions = false
                state.catalogIsCurrent = catalog.project == state.selection.project && catalog.error == nil
                state.optionsError = catalog.error
                return .none
            case .optionsFailed(let error, let epoch, let generation):
                guard epoch == state.presentationEpoch, generation == state.catalogGeneration else { return .none }
                state.isLoadingOptions = false
                state.catalogIsCurrent = false
                state.optionsError = error
                return .none
            case .issuesLoaded(let list, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.isLoadingIssues = false
                state.issuesLoaded = true
                state.issues = list.issues
                state.issuesError = list.error
                return .none
            case .issuesFailed(let error, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.isLoadingIssues = false
                state.issuesError = error
                return .none
            case .submitTapped:
                guard state.submitEnabled else { return .none }
                state.isSubmitting = true
                state.submissionError = nil
                let body = state.selection.body(text: state.text.trimmingCharacters(in: .whitespacesAndNewlines))
                let attachments = state.attachments
                let epoch = state.presentationEpoch
                let useCase = CreateRequestUseCase(operations: operations)
                return .run { send in
                    await send(.submitFinished(await useCase.run(body, attachments), epoch))
                }.cancellable(id: CancelID.submit)
            case .submitFinished(let result, let epoch):
                guard epoch == state.presentationEpoch else { return .none }
                state.isSubmitting = false
                switch result {
                case .confirmed:
                    state.text = ""
                    state.attachments = []
                    state.successfulSubmissionCount += 1
                case .failed(let message):
                    state.submissionError = message
                case .uncertainCommand(let id, let message):
                    state.uncertainCommandID = id
                    state.submissionError = message
                    state.submissionUncertain = true
                case .uncertain(let message):
                    state.submissionError = message
                    state.submissionUncertain = true
                case .cancelled:
                    break
                }
                return .none
            case .commandResolved(let id, let rejection):
                guard state.submissionUncertain, state.uncertainCommandID == id else { return .none }
                state.uncertainCommandID = nil
                state.submissionUncertain = false
                state.submissionError = rejection
                if rejection == nil {
                    state.text = ""
                    state.attachments = []
                    state.successfulSubmissionCount += 1
                }
                return .none
            case .discardUncertainDraft:
                guard state.submissionUncertain else { return .none }
                state.text = ""
                state.attachments = []
                state.submissionError = nil
                state.uncertainCommandID = nil
                state.submissionUncertain = false
                return .none
            }
        }
    }

    private func loadOptions(project: String?, epoch: UInt64, generation: UInt64) -> Effect<Action> {
        .run { send in
            do { await send(.optionsLoaded(try await operations.options(project), epoch, generation)) }
            catch { await send(.optionsFailed(error.localizedDescription, epoch, generation)) }
        }.cancellable(id: CancelID.options, cancelInFlight: true)
    }

    private func loadIssues(state: inout State, refresh: Bool) -> Effect<Action> {
        state.isLoadingIssues = true
        let epoch = state.presentationEpoch
        return .run { send in
            do { await send(.issuesLoaded(try await operations.issues(refresh), epoch)) }
            catch { await send(.issuesFailed(error.localizedDescription, epoch)) }
        }.cancellable(id: CancelID.issues, cancelInFlight: true)
    }
}
