import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// A part of Settings the daemon draws: it observes one view, shows its sections and runs their
/// actions through `ActionFeature`, as every server-driven screen does. The only links it handles
/// itself are the MCP sign-in and reconnect, which open the sign-in sheet here: Settings is a sheet
/// of its own, and another sheet can only open from it.
@Reducer public struct ServerSettingsFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public let key: ViewKey
        public var document: ViewDocument?
        public var actions: ActionFeature.State
        public var notice: String?
        public var error: String?
        @Presents public var signIn: McpSignInFeature.State?

        public init(key: ViewKey, client: ViewClient = .ios, lang: String = NodeLimitFeature.State.deviceLanguage) {
            self.key = key
            actions = ActionFeature.State(dataEpoch: "", client: client, lang: lang)
        }
    }

    public enum Action {
        case task, stop
        case loaded(ViewDocument)
        case perform(WireAction)
        case follow(AlondraContract.Link)
        case actions(ActionFeature.Action)
        case reconnected(McpSignInFeature.Failure?)
        case signIn(PresentationAction<McpSignInFeature.Action>)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.mcpSignIn) var mcpSignIn
    private enum CancelID { case observe }
    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                let key = state.key
                return .run { [viewsClient] send in
                    do {
                        for try await fetch in viewsClient.observe(key) {
                            if case .document(let document) = fetch { await send(.loaded(document)) }
                        }
                    } catch {}
                }
                .cancellable(id: CancelID.observe, cancelInFlight: true)
            case .stop:
                return .cancel(id: CancelID.observe)
            case .loaded(let document):
                state.document = document
                state.actions.dataEpoch = document.dataEpoch
                return .none
            case .perform(let action):
                state.notice = nil
                state.error = nil
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.submit)))
            case .follow(.local(.mcpSignIn(let target, let server))):
                state.signIn = McpSignInFeature.State(card: target, server: server)
                return .none
            case .follow(.local(.mcpReconnect(let target))):
                state.notice = nil
                state.error = nil
                return .run { [mcpSignIn] send in
                    do {
                        try await mcpSignIn.reconnect(target)
                        await send(.reconnected(nil))
                    } catch { await send(.reconnected(McpSignInFeature.failure(of: error, opening: true))) }
                }
            case .follow:
                return .none
            case .reconnected(let failure):
                switch McpSignInFeature.reconnectNotice(failure) {
                case .actionCompleted(let message): state.notice = message
                case .failure(let message): state.error = message
                }
                return .none
            case .actions(.delegate(.completed(let message))):
                state.notice = message
                return .none
            case .actions(.delegate(.failed(let message))):
                state.error = message
                return .none
            case .actions, .signIn:
                return .none
            }
        }
        .ifLet(\.$signIn, action: \.signIn) { McpSignInFeature() }
    }
}
