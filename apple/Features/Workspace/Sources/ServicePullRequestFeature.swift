import AlondraAttachmentsFeature
import AlondraContracts
import AlondraDomain
import AlondraPullRequestsFeature
import AlondraQuestionsFeature
import ComposableArchitecture
import Foundation

/// A pull request card: merge and conflict actions, the diff review, and the questions the
/// server placed under it. Children keep their drafts while a command is outstanding.
@Reducer
public struct ServicePullRequestFeature {
    public init() {}
    @ObservableState
    public struct State: Equatable, Identifiable {
        public var id: DomainV2.PrId { pr.id }
        public var lastKnownCard: DomainPullRequestCard
        public var displayedPR: DomainV2.PullRequest? { pr.pr ?? lastKnownCard.pullRequest }
        /// What GitHub last said about it: title, body, checks, conflicts.
        public var observation: PullRequest? { pr.card?.observation ?? lastKnownCard.observation }
        public var hasOutstandingCommand: Bool {
            pr.isSending || pr.mutationOutcome == .unknown || diff.isSending || diff.mutationOutcome == .unknown
                || questionFeatures.contains { $0.question.isSubmitting || $0.question.needsReconciliation }
        }
        public var downloads = AttachmentDownloadFeature.State()
        public var pr: PullRequestFeature.State
        public var diff: DiffReviewFeature.State
        public var information: [DomainV2.InboxItem] = []
        public var questions: [DomainQuestion] = []
        public var questionFeatures: IdentifiedArrayOf<AttachedPRQuestionFeature.State> = []
        public var visibleQuestionIDs: Set<DomainV2.InteractionId> { Set(questions.map(\.id)) }
        public var descriptionExpanded: Bool?
        public mutating func cancelPendingSubmission() {
            diff.cancelPendingSubmission()
            for id in questionFeatures.ids {
                questionFeatures[id: id]?.question.cancelPendingSubmission()
            }
        }
        public init(card: DomainPullRequestCard, workspace: DomainWorkspace) {
            lastKnownCard = card
            information = card.information
            questions = card.questions
            questionFeatures = IdentifiedArray(uniqueElements: card.questions.map { AttachedPRQuestionFeature.State($0, workspace: workspace) })
            pr = PullRequestFeature.State(card: card, workspace: workspace)
            diff = DiffReviewFeature.State(card: card, workspace: workspace)
        }
    }
    public enum Action {
        case downloads(AttachmentDownloadFeature.Action)
        case pr(PullRequestFeature.Action)
        case diff(DiffReviewFeature.Action)
        case updated(DomainPullRequestCard?, DomainWorkspace?)
        case snapshotUnavailable
        case toggleDescription
        case attachedQuestion(IdentifiedActionOf<AttachedPRQuestionFeature>)
    }
    public var body: some ReducerOf<Self> {
        Scope(state: \.downloads, action: \.downloads) { AttachmentDownloadFeature() }
        Scope(state: \.pr, action: \.pr) { PullRequestFeature() }
        Scope(state: \.diff, action: \.diff) { DiffReviewFeature() }
        Reduce { state, action in
            switch action {
            case .attachedQuestion(.element(let id, .question(.submissionFinished))):
                if !state.visibleQuestionIDs.contains(id), let child = state.questionFeatures[id: id],
                   !child.question.isSubmitting, !child.question.needsReconciliation {
                    state.questionFeatures.remove(id: id)
                }
                return .none
            case .toggleDescription:
                state.descriptionExpanded = !(state.descriptionExpanded ?? ((state.observation?.body?.count ?? 0) <= 600))
                return .none
            case .updated(let card, let workspace):
                var effects: [Effect<Action>] = [.send(.pr(.cardChanged(card, workspace))), .send(.diff(.cardChanged(card, workspace)))]
                guard let card, let workspace else { return .merge(effects) }
                state.lastKnownCard = card
                state.information = card.information
                state.questions = card.questions
                let ids = Set(card.questions.map(\.id))
                for id in state.questionFeatures.ids where !ids.contains(id) {
                    state.questionFeatures[id: id]?.question.cancelPendingSubmission()
                }
                state.questionFeatures.removeAll {
                    !ids.contains($0.id) && !$0.question.isSubmitting && !$0.question.needsReconciliation
                }
                for question in card.questions {
                    if state.questionFeatures[id: question.id] == nil {
                        state.questionFeatures.append(AttachedPRQuestionFeature.State(question, workspace: workspace))
                    } else {
                        effects.append(.send(.attachedQuestion(.element(id: question.id, action: .question(.refreshed(question, workspace))))))
                    }
                }
                return .merge(effects)
            case .diff(.disappeared):
                state.cancelPendingSubmission()
                return .none
            case .snapshotUnavailable: return .merge(.send(.pr(.snapshotUnavailable)), .send(.diff(.snapshotUnavailable)))
            case .pr, .diff, .downloads, .attachedQuestion: return .none
            }
        }
        .forEach(\.questionFeatures, action: \.attachedQuestion) { AttachedPRQuestionFeature() }
    }
}

@Reducer
public struct AttachedPRQuestionFeature {
    @ObservableState
    public struct State: Equatable, Identifiable {
        public var id: DomainV2.InteractionId { question.question.id }
        public var question: QuestionFeature.State
        public init(_ value: DomainQuestion, workspace: DomainWorkspace) {
            question = QuestionFeature.State(question: value, workspace: workspace)
        }
    }
    public enum Action { case question(QuestionFeature.Action) }
    public init() {}
    public var body: some ReducerOf<Self> {
        Scope(state: \.question, action: \.question) { QuestionFeature() }
    }
}
