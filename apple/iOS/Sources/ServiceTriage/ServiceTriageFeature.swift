import AlondraQuestionsFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture

/// Scene-owned navigation. Child drafts survive accepted snapshots of the same card.
@Reducer
struct ServiceTriageFeature {
    @ObservableState
    struct State: Equatable {
        var triage = TriageFeature.State()
        @Presents var question: QuestionFeature.State?
        @Presents var document: DocumentReviewFeature.State?
    }

    enum Action {
        case triage(TriageFeature.Action)
        case question(PresentationAction<QuestionFeature.Action>)
        case document(PresentationAction<DocumentReviewFeature.Action>)
        case open(String)
        case delegate(Delegate)
        enum Delegate { case pullRequest(PullRequest) }
    }

    var body: some ReducerOf<Self> {
        Scope(state: \.triage, action: \.triage) { TriageFeature() }
        Reduce { state, action in
            switch action {
            case .open(let id):
                guard let item = state.triage.routeTarget(for: id) else { return .none }
                switch item {
                case .question(let question):
                    if question.isPlan || (!question.isTerminalMenu && question.attachments.contains(where: \.isText)) {
                        state.document = DocumentReviewFeature.State(subject: .question(question), identity: workIdentity(state))
                    } else {
                        state.question = QuestionFeature.State(question: question)
                    }
                case .report(let report):
                    state.document = DocumentReviewFeature.State(subject: .report(report), identity: workIdentity(state))
                case .pullRequest(let pr): return .send(.delegate(.pullRequest(pr)))
                }
                return .none
            case .triage(.identityChanged):
                state.question = nil
                state.document = nil
                return .none
            case .triage(.inboxUpdated):
                var effects: [Effect<Action>] = []
                if let question = state.question {
                    if let item = state.triage.items.first(where: { $0.id == "q\(question.question.id)" }),
                       case .question(let fresh) = item {
                        effects.append(.send(.question(.presented(.refreshed(fresh)))))
                    } else { state.question = nil }
                }
                if let document = state.document {
                    if let item = state.triage.items.first(where: { $0.id == document.subject.id }) {
                        switch item {
                        case .question(let question): effects.append(.send(.document(.presented(.refreshed(.question(question))))))
                        case .report(let report): effects.append(.send(.document(.presented(.refreshed(.report(report))))))
                        case .pullRequest: break
                        }
                    } else { state.document = nil }
                }
                return .merge(effects)
            case .question(.presented(.submissionFinished)):
                if let question = state.question, question.completed {
                    state.triage.hiddenIDs.insert("q\(question.question.id)")
                    state.question = nil
                }
                return .none
            case .document(.presented(.submissionFinished)), .document(.presented(.reportFeedbackFinished)):
                if let document = state.document, document.completed {
                    state.triage.hiddenIDs.insert(document.subject.id)
                    state.document = nil
                }
                return .none
            case .triage, .question, .document, .delegate: return .none
            }
        }
        .ifLet(\.$question, action: \.question) { QuestionFeature() }
        .ifLet(\.$document, action: \.document) { DocumentReviewFeature() }
    }

    private func workIdentity(_ state: State) -> QuestionWorkIdentity? {
        state.triage.identity.map {
            QuestionWorkIdentity(server: $0.server, deviceID: $0.deviceID,
                                 credentialRevision: $0.credentialRevision, generation: $0.generation)
        }
    }
}
