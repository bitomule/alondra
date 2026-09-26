import AlondraQuestionsFeature
import AlondraShared
import AlondraTriageFeature
import ComposableArchitecture
import SwiftUI

/// Scene-owned navigation. Child drafts survive accepted snapshots of the same card.
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
        public var machine: Machine?
        public var machinePresented = false
        public var usage: Usage?
        public var nodes: [Node] = []
        public var tasks: [WorkTask] = []
        public func task(for item: InboxItem) -> WorkTask? { TaskBoard(requests: [], tasks: tasks).task(for: item) }
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
        public enum Delegate { case pullRequest(PullRequest) }
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
                    guard let item = state.triage.routeTarget(for: id) else { return .none }
                    if let existing = state.documents[id: id] {
                        state.documentID = existing.id
                        return .none
                    }
                    switch item {
                    case .question(let question):
                        state.document = DocumentReviewFeature.State(subject: .question(question), identity: workIdentity(state), feedbackAuthor: state.feedbackAuthor)
                    case .report(let report):
                        state.document = DocumentReviewFeature.State(subject: .report(report), identity: workIdentity(state), feedbackAuthor: state.feedbackAuthor)
                    case .pullRequest(let pr): return .send(.delegate(.pullRequest(pr)))
                    }
                    return .none
                case .triage(.identityChanged(let identity)):
                    guard state.cardIdentity != identity else { return .none }
                    state.cardIdentity = identity
                    state.cards = []
                    state.feedbackAuthor = ""
                    state.machine = nil; state.usage = nil; state.nodes = []; state.tasks = []
                    state.question = nil
                    state.documentID = nil
                    state.documents = []
                    return .none
                case .triage(.inboxUpdated(let update)):
                    guard update.identity == state.triage.identity else { return .none }
                    state.cardIdentity = update.identity
                    state.feedbackAuthor = update.overview?.user ?? ""
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
                            if case .pullRequest(let pr) = item {
                                effects.append(.send(.cards(.element(id: item.id, action: .pullRequest(.contextUpdated(
                                    reports: state.triage.reports(forPullRequest: pr.id),
                                    questions: state.triage.questions(forPullRequest: pr.id)))))))
                            }
                        }
                    }
                    let feedbackAuthor = state.feedbackAuthor
                    for id in state.cards.ids { state.cards[id: id]?.document?.feedbackAuthor = feedbackAuthor }
                    for id in state.documents.ids { state.documents[id: id]?.feedbackAuthor = feedbackAuthor }
                    if let question = state.question {
                        if let item = state.triage.items.first(where: { $0.id == "q\(question.question.id)" }),
                           case .question(let fresh) = item {
                            effects.append(.send(.question(.presented(.refreshed(fresh)))))
                        } else { state.question = nil }
                    }
                    for document in state.documents {
                        if let item = state.triage.items.first(where: { $0.id == document.id }) {
                            switch item {
                            case .question(let question): effects.append(.send(.documents(.element(id: document.id, action: .refreshed(.question(question))))))
                            case .report(let report): effects.append(.send(.documents(.element(id: document.id, action: .refreshed(.report(report))))))
                            case .pullRequest: break
                            }
                        } else if update.overview != nil && !document.isSubmitting && !document.needsReconciliation && !document.feedbackReadRetryAvailable {
                            state.documents.remove(id: document.id)
                            if state.documentID == document.id { state.documentID = nil }
                        }
                    }
                    return .merge(effects)
                case .question(.presented(.submissionFinished)), .question(.presented(.commandResolved)):
                    if let question = state.question, question.completed {
                        state.triage.hiddenIDs.insert("q\(question.question.id)")
                        state.question = nil
                    }
                    return .none
                case .documentDismissed:
                    if let id = state.documentID { state.documents[id: id]?.cancelPendingSubmission() }
                    state.documentID = nil
                    return .none
                case .documents(.element(let id, .submissionFinished)),
                     .documents(.element(let id, .reportFeedbackFinished)),
                     .documents(.element(let id, .commandResolved)):
                    if state.documents[id: id]?.completed == true {
                        state.triage.hiddenIDs.insert(id)
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
