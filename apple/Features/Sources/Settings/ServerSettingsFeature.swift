import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// A part of Settings the daemon draws: it observes one view, shows its sections and runs their
/// actions through `ActionFeature`, as every server-driven screen does. The only links it handles
/// itself are the MCP sign-in and reconnect, which open the sign-in sheet here (Settings is a sheet
/// of its own, and another sheet can only open from it).
@Reducer public struct ServerSettingsFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public let key: ViewKey
        public var document: ViewDocument?
        public var actions: ActionFeature.State
        public var notice: String?
        public var error: String?
        @Presents public var signIn: McpSignInFeature.State?
        public var accountDetail: AlondraContract.Row?
        public var afterAccountDetail: AccountDetailCommand?
        public var orderDraft: AccountOrderDraft?
        /// The key typed into the open account's key field. It starts empty every time an account
        /// opens, and is emptied the moment it is sent and when the account closes.
        public var credentialDraft = ""
        /// What became of the last key sent or removed from the open account.
        public var credentialSend: CredentialSend?

        /// The open account's action that sets its key, and the one that removes it.
        public var credentialActions: (set: WireAction?, clear: WireAction?) {
            let actions = accountDetail?.actions.filter { $0.command == "account_credential" } ?? []
            return (actions.first { $0.optionId == nil }, actions.first { $0.optionId == "clear" })
        }

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
        case accountOpened(String), accountDetailPresented(AlondraContract.Row?)
        case fromAccountDetail(AccountDetailCommand), accountDetailClosed
        case orderBegan, orderMoved(String, to: Int), orderRowsMoved(IndexSet, to: Int), orderSaved
        case orderDraftPresented(AccountOrderDraft?)
        case credentialDraftChanged(String), credentialSaved, credentialRemoved, credentialSent
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
                state.forgetCredential()
                return .none
            case .accountDetailPresented(let row):
                if row?.id != state.accountDetail?.id { state.forgetCredential() }
                state.accountDetail = row
                return .none
            case .fromAccountDetail(let command):
                state.afterAccountDetail = command
                state.accountDetail = nil
                state.forgetCredential()
                return .none
            case .credentialDraftChanged(let text):
                state.credentialDraft = text
                return .none
            case .credentialSaved:
                let key = state.credentialDraft.trimmingCharacters(in: .whitespacesAndNewlines)
                guard let action = state.credentialActions.set, action.enabled, !key.isEmpty, state.credentialSend?.phase != .sending,
                      !(state.actions.inFlight && state.actions.stamp == nil) else { return .none }
                state.credentialDraft = ""
                state.credentialSend = CredentialSend(phase: .sending, removing: false)
                state.notice = nil; state.error = nil
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.textChanged(key))), .send(.actions(.submit)), .send(.credentialSent))
            case .credentialRemoved:
                guard let action = state.credentialActions.clear, action.enabled, state.credentialSend?.phase != .sending,
                      !(state.actions.inFlight && state.actions.stamp == nil) else { return .none }
                state.credentialDraft = ""
                state.credentialSend = CredentialSend(phase: .sending, removing: true)
                state.notice = nil; state.error = nil
                let confirmed: Effect<Action> = action.confirm == nil ? .none : .send(.actions(.confirm))
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.submit)), confirmed, .send(.credentialSent))
            case .credentialSent:
                // The request holds the key until the Mac answers; the field and the draft do not.
                state.actions.text = ""
                guard let pending = state.actions.pending, state.actions.action?.command == "account_credential" else {
                    let reason = state.actions.error ?? OutcomeStyle.transportFailure
                    if state.credentialSend?.phase == .sending { state.credentialSend?.phase = .failed(reason) }
                    return .none
                }
                state.credentialSend?.commandNonce = ActionRequest.deliveryNonce(pending.commandId)
                return .none
            case let .actions(.answered(request, reply)) where state.credentialSend?.answers(request) == true:
                switch OutcomeStyle(reply) {
                case .success(let message): state.credentialSend?.phase = .done(message)
                case .failure(let message): state.credentialSend?.phase = .failed(message)
                case .ask(let changed): state.credentialSend?.phase = .failed(changed.title)
                }
                return .none
            case let .actions(.undelivered(request, failure)) where state.credentialSend?.answers(request) == true:
                state.credentialSend?.phase = .failed(failure.message)
                return .none
            case .accountDetailClosed:
                state.forgetCredential()
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
            case .follow(.local(.accountAdd)), .follow(.local(.accountSignIn)):
                return .none
            case .follow:
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
}


extension ServerSettingsFeature.State {
    mutating func forgetCredential() {
        credentialDraft = ""
        credentialSend = nil
    }
}

/// A key sent or removed from an account's settings, and how the daemon answered it. It never holds
/// the key: only the delivery of the request that carries it.
public struct CredentialSend: Equatable, Sendable {
    public enum Phase: Equatable, Sendable {
        case sending
        /// The daemon stored or removed the key and checked the account again; the words are its own.
        case done(String)
        case failed(String)
    }
    public var phase: Phase
    /// Whether the key was being removed rather than set.
    public let removing: Bool
    public var commandNonce: String?
    public init(phase: Phase, removing: Bool, commandNonce: String? = nil) { self.phase = phase; self.removing = removing; self.commandNonce = commandNonce }

    func answers(_ request: ActionRequest) -> Bool {
        commandNonce != nil && commandNonce == ActionRequest.deliveryNonce(request.commandId)
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
