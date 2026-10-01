import AlondraAttachmentsFeature
import AlondraViews
import AlondraViewsDependency
import AlondraViewsUI
import ComposableArchitecture
import Foundation

/// The ordered queue belongs to the home view. Card details are loaded from their own views.
@Reducer
public struct HomeFeature {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public init() {}
        public var key = ViewKey.home
        public var document: ViewDocument?
        public var status: ViewStatus?
        public var cards: [String: Card] = [:]
        public var loadingCardIDs: Set<String> = []
        public var orderedIDs: [String] = []
        public var index = 0
        public var loading = false
        public var error: String?
        public var actions = ActionFeature.State(dataEpoch: "", client: .ios,
            lang: Locale.preferredLanguages.first?.split(separator: "-").first == "es" ? "es" : "en")
        public var attachments = AttachmentFeature.State()
        public var awaitingAttachments = false

        public var currentID: String? { orderedIDs.indices.contains(index) ? orderedIDs[index] : nil }
        public var currentCard: Card? { currentID.flatMap { cards[$0] } }
        public var badge: Int { Int(status?.badge ?? document?.status.badge ?? 0) }
    }

    public enum Action {
        case appeared
        case disappeared
        case focus(String)
        case stale([String])
        case feedStatus(ViewStatus)
        case reset
        case loaded(ViewFetch)
        case loadFailed(String)
        case dismissError
        case move(Int)
        case cardLoaded(String, ViewFetch)
        case cardFailed(String, String)
        case reloadAfterStamp
        case submitSelected
        case actions(ActionFeature.Action)
        case attachments(AttachmentFeature.Action)
    }

    @Dependency(\.viewsClient) var viewsClient
    private enum CancelID: Hashable { case home, card(String) }

    public var body: some ReducerOf<Self> {
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Scope(state: \.attachments, action: \.attachments) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .appeared:
                state.loading = true
                state.error = nil
                return loadHome(state.key, known: state.document?.version)
            case .disappeared:
                state.awaitingAttachments = false
                let cardCancels = state.loadingCardIDs.map { Effect<Action>.cancel(id: CancelID.card($0)) }
                state.loadingCardIDs = []
                return .merge([.cancel(id: CancelID.home)] + cardCancels + [
                              .run { [viewsClient] _ in await viewsClient.closeScreen("ios.home") },
                              .run { [viewsClient] _ in await viewsClient.closeScreen("ios.home.card") },
                              AttachmentFeature.cancelEffects(instanceID: state.attachments.instanceID).map { .attachments($0) }])
            case let .focus(id):
                state.key = .focusedHome(id)
                state.document = nil
                state.orderedIDs = []
                state.index = 0
                return loadHome(state.key, known: nil)
            case let .stale(keys):
                if keys.contains(state.key.rawValue) { return loadHome(state.key, known: state.document?.version) }
                guard let id = state.currentID, keys.contains(ViewKey.card(id).rawValue) else { return .none }
                guard !state.loadingCardIDs.contains(id) else { return .none }
                state.loadingCardIDs.insert(id)
                return loadCard(id)
            case let .feedStatus(status):
                state.status = status
                return .none
            case .reset:
                state.document = nil
                state.status = nil
                state.cards = [:]
                state.loadingCardIDs = []
                state.orderedIDs = []
                state.index = 0
                return .merge(loadHome(state.key, known: nil), trackCurrentCard(state))
            case let .loaded(.document(document)):
                guard document.key == state.key.rawValue else { return .none }
                let oldID = state.currentID
                let section = document.sections.first { $0.id == "queue" }
                let ids = section?.cardIds ?? []
                let epochChanged = state.document?.dataEpoch != document.dataEpoch
                let cardCancels = epochChanged ? state.loadingCardIDs.map {
                    Effect<Action>.cancel(id: CancelID.card($0))
                } : []
                if epochChanged {
                    state.loadingCardIDs = []
                    state.cards = [:]
                }
                state.document = document
                state.status = document.status
                state.orderedIDs = ids
                state.cards = state.cards.filter { ids.contains($0.key) }
                state.index = oldID.flatMap { ids.firstIndex(of: $0) } ?? 0
                state.actions.dataEpoch = document.dataEpoch
                state.loading = false
                state.error = nil
                return .merge(cardCancels + [prefetch(&state), trackCurrentCard(state)])
            case .loaded(.unchanged):
                state.loading = false
                return .none
            case let .loadFailed(message):
                state.loading = false
                state.error = message
                return .none
            case .dismissError:
                state.error = nil
                return .none
            case let .move(delta):
                guard !state.orderedIDs.isEmpty else { return .none }
                let previous = state.index
                state.index = min(max(state.index + delta, 0), state.orderedIDs.count - 1)
                guard previous != state.index else { return .none }
                state.awaitingAttachments = false
                return .merge(prefetch(&state), trackCurrentCard(state),
                              .send(.actions(.dismiss)), .send(.attachments(.reset)))
            case let .cardLoaded(id, .document(document)):
                state.loadingCardIDs.remove(id)
                guard document.key == ViewKey.card(id).rawValue,
                      document.dataEpoch == state.document?.dataEpoch else {
                    return loadHome(state.key, known: nil)
                }
                guard state.orderedIDs.contains(id),
                      let card = document.sections.lazy.flatMap(\.items).compactMap({ item -> Card? in
                          guard case let .card(card) = item else { return nil }
                          return card
                      }).first(where: { $0.id == id }) else { return .none }
                state.cards[id] = card
                state.actions.dataEpoch = document.dataEpoch
                return .merge(prefetch(&state), trackCurrentCard(state))
            case let .cardLoaded(id, .unchanged):
                state.loadingCardIDs.remove(id)
                return .none
            case let .cardFailed(id, message):
                state.loadingCardIDs.remove(id)
                if state.currentID == id { state.error = message }
                return .none
            case .reloadAfterStamp:
                return loadHome(state.key, known: nil)
            case .submitSelected:
                guard state.actions.action != nil else { return .none }
                guard state.attachments.canPrepareSubmission else {
                    state.actions.error = state.attachments.importError
                    return .none
                }
                if !state.attachments.allReady {
                    state.awaitingAttachments = true
                    return .send(.attachments(.prepareForSubmission))
                }
                state.awaitingAttachments = false
                return .concatenate(
                    .send(.actions(.attachmentsChanged(state.attachments.storedIDs))),
                    .send(.actions(.submit))
                )
            case .attachments:
                guard state.awaitingAttachments else { return .none }
                if let failure = state.attachments.failedUploadMessage {
                    state.awaitingAttachments = false
                    state.actions.error = failure
                    return .none
                }
                return state.attachments.allReady ? .send(.submitSelected) : .none
            case .actions(.response(let response)):
                guard response.outcome == .applied else { return .none }
                return .merge(.send(.attachments(.reset)), .run { send in
                    try await Task.sleep(for: .milliseconds(650))
                    await send(.reloadAfterStamp)
                })
            case .actions(.present):
                state.awaitingAttachments = false
                return .send(.attachments(.reset))
            case .actions:
                return .none
            }
        }
    }

    private func loadHome(_ key: ViewKey, known: String?) -> Effect<Action> {
        .run { [viewsClient] send in
            await viewsClient.openScreen("ios.home", [key])
            do { await send(.loaded(try await viewsClient.load(key, known))) }
            catch is CancellationError { return }
            catch { await send(.loadFailed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
        }.cancellable(id: CancelID.home, cancelInFlight: true)
    }

    private func prefetch(_ state: inout State) -> Effect<Action> {
        guard !state.orderedIDs.isEmpty else { return .none }
        let targets = state.orderedIDs[state.index...].prefix(2).filter {
            state.cards[$0] == nil && !state.loadingCardIDs.contains($0)
        }
        state.loadingCardIDs.formUnion(targets)
        return .merge(targets.map(loadCard))
    }

    private func loadCard(_ id: String) -> Effect<Action> {
        .run { [viewsClient] send in
            do { await send(.cardLoaded(id, try await viewsClient.load(.card(id), nil))) }
            catch is CancellationError { return }
            catch { await send(.cardFailed(id, String(localized: "Couldn’t load this from your Mac. Try again."))) }
        }.cancellable(id: CancelID.card(id), cancelInFlight: true)
    }

    private func trackCurrentCard(_ state: State) -> Effect<Action> {
        let key: ViewKey? = state.currentID.flatMap { state.cards[$0] == nil ? nil : .card($0) }
        return .run { [viewsClient] _ in
            await viewsClient.openScreen("ios.home.card", Set(key.map { [$0] } ?? []))
        }
    }
}
