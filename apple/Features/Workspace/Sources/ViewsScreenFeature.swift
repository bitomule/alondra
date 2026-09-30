import AlondraViews
import AlondraViewsDependency
import AlondraViewsUI
import ComposableArchitecture
import Foundation

/// Loads one server-owned screen and keeps its feed registration scoped to this presentation.
@Reducer
public struct ViewsScreenFeature {
    @ObservableState
    public struct State: Equatable {
        public var key: ViewKey
        public let owner: String
        public var document: ViewDocument?
        public var parentKey: ViewKey?
        public var loading = false
        public var isPresented = false
        public var error: String?
        public var content: String?
        public var attachmentPreview: ViewsAttachmentPreview?
        public var action: ActionFeature.State

        public init(key: ViewKey, owner: String) {
            self.key = key
            self.owner = owner
            self.action = ActionFeature.State(dataEpoch: "", client: .ios, lang: "en")
        }
    }

    public enum Action {
        case appeared, disappeared, back, refresh
        case stale([String]), reset
        case open(ViewKey)
        case loaded(ViewFetch), failed(String)
        case fetchContent(ContentRef), contentLoaded(String), contentFailed(String)
        case fetchAttachment(AlondraViews.Attachment), attachmentLoaded(AlondraViews.Attachment, Data), closeAttachment, dismissError
        case action(ActionFeature.Action)
    }

    @Dependency(\.viewsClient) var viewsClient
    private enum CancelID { case load }

    public var body: some ReducerOf<Self> {
        Scope(state: \.action, action: \.action) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .appeared:
                state.isPresented = true
                state.loading = true
                state.error = nil
                return load(state)
            case .refresh:
                state.loading = true
                state.error = nil
                return load(state, known: nil)
            case .disappeared:
                state.isPresented = false
                return .merge(.cancel(id: CancelID.load), .run { [viewsClient, owner = state.owner] _ in
                    await viewsClient.closeScreen(owner)
                })
            case .back:
                guard let parentKey = state.parentKey else { return .none }
                state.key = parentKey
                state.parentKey = nil
                state.loading = true
                return load(state)
            case .open(let key):
                guard state.isPresented else { return .none }
                guard key != state.key else { return .none }
                state.parentKey = state.key
                state.key = key
                state.document = nil
                state.content = nil
                state.loading = true
                state.error = nil
                return load(state)
            case .stale(let keys):
                guard state.isPresented, keys.contains(state.key.rawValue) else { return .none }
                state.loading = true
                return load(state)
            case .reset:
                guard state.isPresented else { return .none }
                state.document = nil
                state.loading = true
                return load(state)
            case .loaded(.document(let document)):
                guard document.key == state.key.rawValue else { return .none }
                state.document = document
                state.action.dataEpoch = document.dataEpoch
                state.loading = false
                state.error = nil
                return .none
            case .loaded(.unchanged):
                state.loading = false
                return .none
            case .failed(let message):
                state.loading = false
                state.error = message
                return .none
            case .fetchContent(let reference):
                return .run { [viewsClient] send in
                    do { await send(.contentLoaded(try await viewsClient.fetchContent(reference))) }
                    catch { await send(.contentFailed(String(describing: error))) }
                }
            case .contentLoaded(let value):
                state.content = value
                return .none
            case .contentFailed(let message):
                state.error = message
                return .none
            case .fetchAttachment(let attachment):
                return .run { [viewsClient] send in
                    do { await send(.attachmentLoaded(attachment, try await viewsClient.fetchAttachment(attachment))) }
                    catch { await send(.contentFailed(String(describing: error))) }
                }
            case .attachmentLoaded(let attachment, let data):
                state.attachmentPreview = ViewsAttachmentPreview(attachment: attachment, data: data)
                return .none
            case .closeAttachment:
                state.attachmentPreview = nil
                return .none
            case .dismissError:
                state.error = nil
                return .none
            case .action(.response(let response)) where response.outcome == .applied:
                state.loading = true
                return .merge(load(state), .send(.action(.dismiss)))
            case .action:
                return .none
            }
        }
    }

    private func load(_ state: State, known: String? = nil) -> Effect<Action> {
        let key = state.key
        let owner = state.owner
        let version = known ?? state.document?.version
        return .run { [viewsClient] send in
            await viewsClient.openScreen(owner, [key])
            do { await send(.loaded(try await viewsClient.load(key, version))) }
            catch { await send(.failed(String(describing: error))) }
        }.cancellable(id: CancelID.load, cancelInFlight: true)
    }
}

public struct ViewsAttachmentPreview: Equatable, Identifiable {
    public let attachment: AlondraViews.Attachment
    public let data: Data
    public var id: String { attachment.id }
}
