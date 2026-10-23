import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// A part of Settings the daemon draws: it observes one view, shows its sections and runs their
/// actions through `ActionFeature`, as every server-driven screen does. The only links it handles
/// itself are the MCP sign-in and reconnect, which open the sign-in sheet here (Settings is a sheet
/// of its own, and another sheet can only open from it), and the Claude account ones, whose command
/// runs in Terminal on the daemon's Mac.
@Reducer public struct ServerSettingsFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public let key: ViewKey
        public var document: ViewDocument?
        public var actions: ActionFeature.State
        public var notice: String?
        public var error: String?
        @Presents public var signIn: McpSignInFeature.State?
        public var newAccount: NewAccount?
        public var claudeSignIn: ClaudeSignIn?

        public init(key: ViewKey, client: ViewClient = .ios, lang: String = NodeLimitFeature.State.deviceLanguage) {
            self.key = key
            actions = ActionFeature.State(dataEpoch: "", client: client, lang: lang)
        }
    }

    public enum Action {
        case task, stop
        case loaded(ViewDocument)
        case perform(WireAction)
        case performOption(WireAction, optionID: String)
        case follow(AlondraContract.Link)
        case actions(ActionFeature.Action)
        case reconnected(McpSignInFeature.Failure?)
        case signIn(PresentationAction<McpSignInFeature.Action>)
        case newAccountNameChanged(String), newAccountAdded, newAccountCancelled
        case terminalAnswered(command: String, opened: Bool)
        case claudeSignInPresented(ClaudeSignIn?)
    }

    /// The name the person gives a new Claude account before it signs in.
    public struct NewAccount: Equatable, Sendable {
        public let command: String
        public var name = ""
        public var canAdd: Bool { !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        public init(command: String, name: String = "") { self.command = command; self.name = name }
    }

    /// A Claude sign-in left for the person to finish in Terminal: on this Mac, where Terminal is
    /// open with the command copied, or on the daemon's Mac.
    public struct ClaudeSignIn: Equatable, Identifiable, Sendable {
        public let command: String
        public let terminalOpen: Bool
        public var id: String { command }
        public init(command: String, terminalOpen: Bool) { self.command = command; self.terminalOpen = terminalOpen }
    }

    public static func shellQuoted(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.mcpSignIn) var mcpSignIn
    @Dependency(\.claudeTerminal) var claudeTerminal
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
            case let .performOption(action, optionID):
                state.notice = nil
                state.error = nil
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.chooseOption(optionID))), .send(.actions(.submit)))
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
            case .follow(.local(.accountAdd(let command))):
                state.newAccount = NewAccount(command: command)
                return .none
            case .follow(.local(.accountSignIn(_, let command))):
                return openTerminal(command)
            case .follow:
                return .none
            case .newAccountNameChanged(let name):
                state.newAccount?.name = name
                return .none
            case .newAccountAdded:
                guard let account = state.newAccount, account.canAdd else { return .none }
                state.newAccount = nil
                let name = account.name.trimmingCharacters(in: .whitespacesAndNewlines)
                return openTerminal("\(account.command) \(Self.shellQuoted(name))")
            case .newAccountCancelled:
                state.newAccount = nil
                return .none
            case let .terminalAnswered(command, opened):
                state.claudeSignIn = ClaudeSignIn(command: command, terminalOpen: opened)
                return .none
            case .claudeSignInPresented(let signIn):
                state.claudeSignIn = signIn
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

    private func openTerminal(_ command: String) -> Effect<Action> {
        .run { [claudeTerminal] send in
            await send(.terminalAnswered(command: command, opened: await claudeTerminal.open(command)))
        }
    }
}
