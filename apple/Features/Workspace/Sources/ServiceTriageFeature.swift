import AlondraQuestionsFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture

/// Scene-owned navigation. Child drafts survive accepted snapshots of the same card.
@Reducer
public struct ServiceTriageFeature {
    public init() {}
    @ObservableState
    public struct State: Equatable {
        public init() {}
        public var triage = TriageFeature.State()
        public var cards: IdentifiedArrayOf<TriageCardFeature.State> = []
        public var cardIdentity: TriageIdentity?
        public var machine: Machine?
        public var machinePresented = false
        public var usage: Usage?
        public var nodes: [Node] = []
        public var tasks: [WorkTask] = []
        public func task(for item: InboxItem) -> WorkTask? { TaskBoard(requests: [], tasks: tasks).task(for: item) }
        @Presents public var question: QuestionFeature.State?
        @Presents public var document: DocumentReviewFeature.State?
    }

    public enum Action {
        case triage(TriageFeature.Action)
        case cards(IdentifiedActionOf<TriageCardFeature>)
        case question(PresentationAction<QuestionFeature.Action>)
        case document(PresentationAction<DocumentReviewFeature.Action>)
        case open(String)
        case machinePresented(Bool)
        case delegate(Delegate)
        public enum Delegate { case pullRequest(PullRequest) }
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.triage, action: \.triage) { TriageFeature() }
        Reduce { state, action in
            switch action {
            case .machinePresented(let value):
                state.machinePresented = value
                return .none
            case .open(let id):
                guard let item = state.triage.routeTarget(for: id) else { return .none }
                switch item {
                case .question(let question):
                    state.document = DocumentReviewFeature.State(subject: .question(question), identity: workIdentity(state))
                case .report(let report):
                    state.document = DocumentReviewFeature.State(subject: .report(report), identity: workIdentity(state))
                case .pullRequest(let pr): return .send(.delegate(.pullRequest(pr)))
                }
                return .none
            case .triage(.identityChanged(let identity)):
                guard state.cardIdentity != identity else { return .none }
                state.cardIdentity = identity
                state.cards = []
                state.machine = nil; state.usage = nil; state.nodes = []; state.tasks = []
                state.question = nil
                state.document = nil
                return .none
            case .triage(.inboxUpdated(let update)):
                guard update.identity == state.triage.identity else { return .none }
                state.cardIdentity = update.identity
                var effects: [Effect<Action>] = []
                if update.identity == state.triage.identity, let overview = update.overview {
                    state.machine = overview.machine; state.usage = overview.usage
                    state.nodes = overview.nodes; state.tasks = overview.tasks ?? []
                    let ids = Set(state.triage.items.map(\.id))
                    state.cards.removeAll { !ids.contains($0.id) }
                    for item in state.triage.items {
                        if state.cards[id: item.id] == nil {
                            state.cards.append(TriageCardFeature.State(item: item, identity: workIdentity(state)))
                        } else {
                            effects.append(.send(.cards(.element(id: item.id, action: .refreshed(item)))))
                        }
                    }
                }
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
            case .question(.presented(.submissionFinished)), .question(.presented(.commandResolved)):
                if let question = state.question, question.completed {
                    state.triage.hiddenIDs.insert("q\(question.question.id)")
                    state.question = nil
                }
                return .none
            case .document(.presented(.submissionFinished)), .document(.presented(.reportFeedbackFinished)), .document(.presented(.commandResolved)):
                if let document = state.document, document.completed {
                    state.triage.hiddenIDs.insert(document.subject.id)
                    state.document = nil
                }
                return .none
            case .cards(.element(_, .delegate(.resolved(let id)))):
                state.triage.hiddenIDs.insert(id)
                state.triage.resolvedCount += 1
                return .none
            case .cards(.element(_, .delegate(.hide(let id)))):
                return .send(.triage(.hide(id)))
            case .triage, .question, .document, .delegate, .cards: return .none
            }
        }
        .forEach(\.cards, action: \.cards) { TriageCardFeature() }
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
