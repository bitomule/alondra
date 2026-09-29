import AlondraContracts
import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture
import SwiftUI

/// Display context around a card: who works on it, which Task, when it arrived and the request.
public struct CardContext: Equatable, Sendable {
    public let node: Node?
    public let agent: DomainV2.Agent?
    public let task: DomainV2.Task?
    public let date: Int64?
    public let mission: String?
}

/// Scene-owned navigation. Child drafts survive accepted views of the same card.
@Reducer
public struct ServiceTriageFeature {
    public init() {}
    @ObservableState
    public struct State: Equatable {
        public init() {}
        public mutating func cancelPendingSubmissions() {
            for id in cards.ids { cards[id: id]?.cancelPendingSubmission() }
            question?.cancelPendingSubmission()
            for id in documents.ids { documents[id: id]?.cancelPendingSubmission() }
        }
        public var feedbackAuthor = ""
        public var triage = TriageFeature.State()
        public var cardStack = CardStackFeature.State()
        public var keyboardVisible = false
        public func isLeaving(_ id: String) -> Bool { cards[id: id]?.stamp != nil || triage.hiddenIDs.contains(id) }
        public var cards: IdentifiedArrayOf<TriageCardFeature.State> = []
        public var cardIdentity: TriageIdentity?
        public var workspace: DomainWorkspace? { triage.workspace }
        public var machine: Machine? { workspace?.runtime?.machine }
        public var usage: Usage? { workspace?.runtime?.usage }
        public var nodes: [Node] { workspace?.runtime?.nodes ?? [] }
        public var machinePresented = false
        public func task(for item: DomainV2.InboxItem) -> DomainV2.Task? { workspace?.task(item.taskId) }
        public func agent(for item: DomainV2.InboxItem) -> DomainV2.Agent? { workspace?.agent(item.agentId) }
        public func context(for item: DomainV2.InboxItem) -> CardContext {
            let interaction: DomainV2.InteractionId? = if case .interaction(let id) = item.subject { id } else { nil }
            return CardContext(node: workspace?.runtime?.node(item.agentId), agent: agent(for: item), task: task(for: item),
                               date: workspace?.runtime?.createdAt(interaction),
                               mission: workspace?.request(item.requestId)?.body)
        }
        @Presents public var question: QuestionFeature.State?
        public var documents: IdentifiedArrayOf<DocumentReviewFeature.State> = []
        public var documentID: String?
        public var document: DocumentReviewFeature.State? {
            get { documentID.flatMap { documents[id: $0] } }
            set {
                if let newValue { documents[id: newValue.id] = newValue; documentID = newValue.id }
                else { documentID = nil }
            }
        }
    }

    public enum Action {
        case triage(TriageFeature.Action)
        case cardStack(CardStackFeature.Action)
        case keyboardVisibilityChanged(Bool)
        case presentationDisappeared
        case cards(IdentifiedActionOf<TriageCardFeature>)
        case question(PresentationAction<QuestionFeature.Action>)
        case documentDismissed
        case documents(IdentifiedActionOf<DocumentReviewFeature>)
        case open(String)
        case machinePresented(Bool)
        case delegate(Delegate)
        public enum Delegate { case pullRequest(DomainPullRequestCard) }
    }

    public var body: some ReducerOf<Self> {
        CombineReducers {
            Reduce { state, action in
                switch action {
                case .triage(.disappeared), .presentationDisappeared, .open:
                    state.cancelPendingSubmissions()
                case .cardStack(.delegate(let departure)):
                    if departure.identity == state.triage.identity, departure.itemID == state.triage.queue.first?.id {
                        state.cards[id: departure.itemID]?.cancelPendingSubmission()
                    }
                default: break
                }
                return .none
            }
            Scope(state: \.triage, action: \.triage) { TriageFeature() }
            Reduce { state, _ in
                if state.cardStack.identity != state.triage.identity {
                    state.cardStack = CardStackFeature.State()
                }
                state.cardStack.frontID = state.triage.queue.first?.id
                state.cardStack.count = state.triage.queue.count
                state.cardStack.identity = state.triage.identity
                return .none
            }
            Scope(state: \.cardStack, action: \.cardStack) { CardStackFeature() }
            Reduce { state, action in
                switch action {
                case .presentationDisappeared: return .none
                case .keyboardVisibilityChanged(let visible):
                    state.keyboardVisible = visible
                    return .none
                case .cardStack(.skipTapped), .cardStack(.dragEnded):
                    // CardStack has already accepted or rejected the gesture. Invalidate
                    // before the view animates; a rejected drag has no departure.
                    if let departure = state.cardStack.departure {
                        state.cards[id: departure.itemID]?.cancelPendingSubmission()
                    }
                    return .none
                case .cardStack(.delegate(let departure)):
                    return .send(.triage(.skipIfCurrent(departure.itemID, departure.identity)), animation: .spring(duration: 0.5, bounce: 0.25))
                case .cardStack: return .none
                case .machinePresented(let value):
                    state.machinePresented = value
                    return .none
                case .open(let id):
                    guard let root = state.triage.routeTarget(for: id), let workspace = state.workspace else { return .none }
                    switch CardContent(root, in: workspace) {
                    case .pullRequest(let card): return .send(.delegate(.pullRequest(card)))
                    case .document(let subject):
                        if state.documents[id: subject.id] != nil { state.documentID = subject.id; return .none }
                        state.document = DocumentReviewFeature.State(subject: subject, workspace: workspace,
                            identity: workIdentity(state), feedbackAuthor: state.feedbackAuthor)
                    case .question(let question):
                        let item = question.item
                        guard let subject = DocumentSubject(item: item, in: workspace) else { return .none }
                        if state.documents[id: subject.id] != nil { state.documentID = subject.id; return .none }
                        state.document = DocumentReviewFeature.State(subject: subject, workspace: workspace,
                            identity: workIdentity(state), feedbackAuthor: state.feedbackAuthor)
                    case nil: return .none
                    }
                    return .none
                case .triage(.identityChanged(let identity)):
                    guard state.cardIdentity != identity else { return .none }
                    state.cardIdentity = identity
                    state.cards = []
                    state.feedbackAuthor = ""
                    state.question = nil
                    state.documentID = nil
                    state.documents = []
                    return .none
                case .triage(.workspaceUpdated(let update)):
                    guard update.identity == state.triage.identity else { return .none }
                    state.cardIdentity = update.identity
                    var effects: [Effect<Action>] = []
                    guard let workspace = update.workspace else { return .none }
                    let ids = Set(state.triage.items.map(\.id))
                    state.cards.removeAll { !ids.contains($0.id) && !$0.retainsPendingDelivery }
                    for item in state.triage.items {
                        if state.cards[id: item.id] == nil {
                            state.cards.append(TriageCardFeature.State(item: item, workspace: workspace, identity: workIdentity(state)))
                        } else {
                            effects.append(.send(.cards(.element(id: item.id, action: .refreshed(item, workspace)))))
                        }
                    }
                    if let question = state.question {
                        if let item = workspace.item(for: question.question.id),
                           let fresh = DomainQuestion(item: item, in: workspace) {
                            effects.append(.send(.question(.presented(.refreshed(fresh, workspace)))))
                        } else if !question.isSubmitting && !question.needsReconciliation { state.question = nil }
                    }
                    for document in state.documents {
                        if let item = workspace.item(document.id), let subject = DocumentSubject(item: item, in: workspace) {
                            effects.append(.send(.documents(.element(id: document.id, action: .refreshed(subject, workspace)))))
                        } else if !document.isSubmitting && !document.needsReconciliation {
                            state.documents.remove(id: document.id)
                            if state.documentID == document.id { state.documentID = nil }
                        }
                    }
                    return .merge(effects)
                case .question(.presented(.submissionFinished)):
                    if let question = state.question, question.completed {
                        state.triage.hiddenIDs.insert(question.question.item.id)
                        state.question = nil
                    }
                    return .none
                case .documentDismissed:
                    if let id = state.documentID { state.documents[id: id]?.cancelPendingSubmission() }
                    state.documentID = nil
                    return .none
                case .documents(.element(let id, .submissionFinished)):
                    if state.documents[id: id]?.completed == true {
                        let root = state.workspace?.root(containing: id)?.id ?? id
                        state.triage.hiddenIDs.insert(root)
                        state.documents.remove(id: id)
                        if state.documentID == id { state.documentID = nil }
                    }
                    return .none
                case .triage(.readAcknowledged(let id, let confirmed)):
                    guard state.cards[id: id] != nil else { return .none }
                    return .send(.cards(.element(id: id, action: .readAcknowledged(confirmed))))
                case .cards(.element(_, .delegate(.read(let id)))):
                    return .send(.triage(.readWithPresentation(id)))
                case .cards(.element(_, .delegate(.readPresentationFinished(let id)))):
                    return .send(.triage(.readPresentationFinished(id)), animation: .spring(duration: 0.55, bounce: 0.2))
                case .cards(.element(_, .delegate(.resolved(let id)))):
                    state.triage.hiddenIDs.insert(id)
                    state.triage.resolvedCount += 1
                    return .none
                case .cards(.element(_, .delegate(.hide(let id)))):
                    return .send(.triage(.hide(id)))
                case .triage, .question, .documents, .delegate, .cards: return .none
                }
            }
            .forEach(\.cards, action: \.cards) { TriageCardFeature() }
            .ifLet(\.$question, action: \.question) { QuestionFeature() }
            .forEach(\.documents, action: \.documents) { DocumentReviewFeature() }
        }
        .onChange(of: \.triage.queue.first?.id) { previous, _ in
            Reduce { state, _ in
                if let previous { state.cards[id: previous]?.cancelPendingSubmission() }
                return .none
            }
        }
    }

    private func workIdentity(_ state: State) -> QuestionWorkIdentity? {
        state.triage.identity.map {
            QuestionWorkIdentity(server: $0.server, deviceID: $0.deviceID,
                                 credentialRevision: $0.credentialRevision, generation: $0.generation)
        }
    }
}
