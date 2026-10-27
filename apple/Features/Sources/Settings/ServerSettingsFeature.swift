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
        public var macName: String?
        public var accountDetail: AlondraContract.Row?
        public var afterAccountDetail: AccountDetailCommand?
        public var orderDraft: AccountOrderDraft?

        public var accountRows: [AlondraContract.Row] {
            document?.sections.flatMap(\.items).compactMap { item in
                if case let .row(row) = item, row.account != nil { row } else { nil }
            } ?? []
        }
        public var reorderAction: WireAction? {
            document?.sections.flatMap(\.actions).first { $0.command == "account_order" && $0.enabled }
        }

        /// The row each action that takes it away (`resolves_card`) hides while it goes, by its command.
        public var hidden: [String: String] = [:]

        public init(key: ViewKey, client: ViewClient = .ios, lang: String = MachineSettingsFeature.State.deviceLanguage) {
            self.key = key
            actions = ActionFeature.State(dataEpoch: "", client: client, lang: lang)
        }

        public var hiddenRows: Set<String> { Set(hidden.values) }
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
        case terminalAnswered(command: String, opened: Bool, provider: AccountProvider = .claude, macName: String? = nil)
        case claudeSignInPresented(ClaudeSignIn?)
        case accountOpened(String), accountDetailPresented(AlondraContract.Row?)
        case fromAccountDetail(AccountDetailCommand), accountDetailClosed
        case orderBegan, orderMoved(String, to: Int), orderRowsMoved(IndexSet, to: Int), orderSaved
        case orderDraftPresented(AccountOrderDraft?)
    }

    /// The name the person gives a new Claude account before it signs in.
    public struct NewAccount: Equatable, Sendable {
        public let command: String
        public let provider: AccountProvider
        public let argv: [String]?
        public var name = ""
        public var canAdd: Bool { !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        public init(command: String, name: String = "", provider: AccountProvider = .claude, argv: [String]? = nil) {
            self.command = command; self.name = name; self.provider = provider; self.argv = argv
        }
    }

    /// A provider sign-in left for the person to finish in Terminal: on this Mac, where Terminal is
    /// open with the command copied, or on the daemon's Mac.
    public struct ClaudeSignIn: Equatable, Identifiable, Sendable {
        public let command: String
        public let terminalOpen: Bool
        public let provider: AccountProvider
        public let macName: String?
        public var id: String { command }
        public init(command: String, terminalOpen: Bool, provider: AccountProvider = .claude, macName: String? = nil) {
            self.command = command; self.terminalOpen = terminalOpen; self.provider = provider; self.macName = macName
        }
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
                let rows = Set(document.sections.flatMap(\.items).compactMap { item in if case .row(let row) = item { row.id } else { nil } })
                state.hidden = state.hidden.filter { rows.contains($0.value) }
                if let id = state.accountDetail?.id { state.accountDetail = state.accountRows.first { $0.id == id } }
                if let draft = state.orderDraft, document.dataEpoch != draft.dataEpoch {
                    state.orderDraft?.conflict = true; state.orderDraft?.saving = false
                } else if let draft = state.orderDraft, !draft.saving {
                    let version = document.accountConfiguration?.version
                    state.orderDraft?.conflict = document.dataEpoch != draft.dataEpoch || version != draft.configurationVersion
                }
                return .none
            case .accountOpened(let id):
                state.accountDetail = state.accountRows.first { $0.id == id }
                return .none
            case .accountDetailPresented(let row):
                state.accountDetail = row
                return .none
            case .fromAccountDetail(let command):
                state.afterAccountDetail = command
                state.accountDetail = nil
                return .none
            case .accountDetailClosed:
                let next = state.afterAccountDetail
                state.afterAccountDetail = nil
                switch next {
                case .perform(let action): return .send(.perform(action))
                case .follow(let link): return .send(.follow(link))
                case let .option(action, id): return .send(.performOption(action, optionID: id))
                case .reorder: return .send(.orderBegan)
                case nil: return .none
                }
            case .orderBegan:
                guard state.orderDraft?.saving != true, let document = state.document, let action = state.reorderAction,
                      let version = document.accountConfiguration?.version, !version.isEmpty,
                      state.accountRows.count > 1 else { return .none }
                state.orderDraft = AccountOrderDraft(rows: state.accountRows, configurationVersion: version, dataEpoch: document.dataEpoch, action: action)
                return .none
            case let .orderMoved(id, destination):
                guard var draft = state.orderDraft, !draft.saving, !draft.conflict,
                      let source = draft.rows.firstIndex(where: { $0.id == id }), draft.rows.indices.contains(destination), source != destination else { return .none }
                draft.rows.insert(draft.rows.remove(at: source), at: destination)
                state.orderDraft = draft
                return .none
            case let .orderRowsMoved(offsets, destination):
                guard var draft = state.orderDraft, !draft.saving, !draft.conflict,
                      !offsets.isEmpty, offsets.allSatisfy(draft.rows.indices.contains), (0...draft.rows.count).contains(destination) else { return .none }
                let moved = offsets.sorted().map { draft.rows[$0] }
                let target = destination - offsets.filter { $0 < destination }.count
                draft.rows = draft.rows.enumerated().filter { !offsets.contains($0.offset) }.map(\.element)
                draft.rows.insert(contentsOf: moved, at: target)
                state.orderDraft = draft
                return .none
            case .orderSaved:
                guard !state.actions.inFlight, var draft = state.orderDraft, !draft.saving, !draft.conflict, draft.hasChanges else { return .none }
                draft.saving = true
                state.orderDraft = draft
                let order = draft.rows.compactMap(\.account).map { account in
                    JSONValue.object(["provider": .string(account.key.provider.rawValue), "account": .string(account.key.account)])
                }
                let fields: [String: JSONValue] = ["order": .array(order), "expected_config_version": .string(draft.configurationVersion)]
                state.error = nil; state.notice = nil
                return .concatenate(.send(.actions(.presentForm(draft.action, fields, requiresAttachments: false))), .send(.actions(.submit)))
            case .orderDraftPresented(let draft):
                guard state.orderDraft?.saving != true else { return .none }
                state.orderDraft = draft
                return .none
            case .perform(let action):
                if action.command == "account_order" { return .send(.orderBegan) }
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
            case let .follow(.local(.accountAdd(command, provider, argv))):
                guard provider == .claude && argv == nil || argv.flatMap({ AccountCommand.validatedArguments($0, provider: provider, adding: true) }) != nil else {
                    state.error = String(localized: "This account action is unavailable. Update Alondra and try again.")
                    return .none
                }
                state.newAccount = NewAccount(command: command, provider: provider, argv: argv)
                return .none
            case let .follow(.local(.accountSignIn(account, command, provider, argv))):
                if let argv {
                    guard provider != .codex || argv.last == account,
                          let command = AccountCommand.command(argv, provider: provider, adding: false) else {
                        state.error = String(localized: "This account action is unavailable. Update Alondra and try again.")
                        return .none
                    }
                    return openTerminal(command, provider: provider, macName: state.macName)
                }
                guard provider == .claude else {
                    state.error = String(localized: "This account action is unavailable. Update Alondra and try again.")
                    return .none
                }
                return openTerminal(command, macName: state.macName)
            case .follow:
                return .none
            case .newAccountNameChanged(let name):
                state.newAccount?.name = name
                return .none
            case .newAccountAdded:
                guard let account = state.newAccount, account.canAdd else { return .none }
                let name = account.name.trimmingCharacters(in: .whitespacesAndNewlines)
                let command: String
                if let argv = account.argv {
                    guard let rendered = AccountCommand.command(argv, provider: account.provider, adding: true, newName: name) else { return .none }
                    command = rendered
                } else {
                    command = "\(account.command) \(Self.shellQuoted(name))"
                }
                state.newAccount = nil
                return openTerminal(command, provider: account.provider, macName: state.macName)
            case .newAccountCancelled:
                state.newAccount = nil
                return .none
            case let .terminalAnswered(command, opened, provider, macName):
                state.claudeSignIn = ClaudeSignIn(command: command, terminalOpen: opened, provider: provider, macName: macName)
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
            case .actions(.submit):
                if state.orderDraft?.saving == true, state.orderDraft?.commandNonce == nil,
                   state.actions.action?.command == "account_order", let request = state.actions.pending {
                    state.orderDraft?.commandNonce = ActionRequest.deliveryNonce(request.commandId)
                }
                return .none
            case let .actions(.answered(request, reply)):
                guard state.orderDraft?.saving == true, state.orderDraft?.commandNonce == ActionRequest.deliveryNonce(request.commandId) else { return .none }
                switch OutcomeStyle(reply) {
                case .success: state.orderDraft = nil
                case .failure, .ask:
                    state.orderDraft?.saving = false
                    state.orderDraft?.conflict = true
                }
                return .none
            case let .actions(.undelivered(request, failure)):
                guard state.orderDraft?.saving == true, state.orderDraft?.commandNonce == ActionRequest.deliveryNonce(request.commandId) else { return .none }
                if failure != .unsure {
                    state.orderDraft?.saving = false
                    state.orderDraft?.conflict = true
                }
                state.error = failure.message
                return .none
            case .actions(.delegate(.completed(let message))):
                if state.orderDraft?.saving != true { state.error = nil }
                state.notice = message
                return .none
            case .actions(.delegate(.failed(let message))):
                state.error = message
                return .none
            case .signIn(.presented(.delegate(.signedIn(let server)))):
                state.error = nil
                state.notice = String(localized: "Signed in to \(server): the waiting tasks go on.")
                return .none
            case .actions(.sending(let request)):
                guard state.actions.action?.resolvesCard == true, let row = state.row(with: request.actionId) else { return .none }
                state.hidden[request.commandId] = row
                return .none
            case .actions(.settled(let request, applied: false)), .actions(.undelivered(let request, _)):
                state.hidden[request.commandId] = nil
                return .none
            case .actions, .signIn:
                return .none
            }
        }
        .ifLet(\.$signIn, action: \.signIn) { McpSignInFeature() }
    }

    private func openTerminal(_ command: String, provider: AccountProvider = .claude, macName: String? = nil) -> Effect<Action> {
        .run { [claudeTerminal] send in
            await send(.terminalAnswered(command: command, opened: await claudeTerminal.open(command), provider: provider, macName: macName))
        }
    }
}


public enum AccountDetailCommand: Equatable, Sendable {
    case perform(WireAction), follow(AlondraContract.Link), option(WireAction, String), reorder
}

public struct AccountOrderDraft: Equatable, Identifiable, Sendable {
    public var rows: [AlondraContract.Row]
    public let initialIDs: [String]
    public let configurationVersion: String
    public let dataEpoch: String
    public let action: WireAction
    public var conflict = false
    public var saving = false
    public var commandNonce: String?
    public var id: String { configurationVersion }
    public var hasChanges: Bool { rows.map(\.id) != initialIDs }
    public init(rows: [AlondraContract.Row], configurationVersion: String, dataEpoch: String, action: WireAction) {
        self.rows = rows; initialIDs = rows.map(\.id); self.configurationVersion = configurationVersion
        self.dataEpoch = dataEpoch; self.action = action
    }
}

private extension ServerSettingsFeature.State {
    func row(with actionID: String) -> String? {
        document?.sections.flatMap(\.items).lazy.compactMap { item -> String? in
            guard case .row(let row) = item, row.actions.contains(where: { $0.id == actionID }) else { return nil }
            return row.id
        }.first
    }
}
