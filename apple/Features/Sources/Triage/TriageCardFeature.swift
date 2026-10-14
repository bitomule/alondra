import AlondraAttachments
import AlondraContract
import ComposableArchitecture
import Foundation
import OSLog

/// One card of Waiting for You: it observes its own view, owns the reply draft, and decides what
/// each control on the card does. The daemon decides which buttons the card has and in which order.
@Reducer
public struct TriageCardFeature: Sendable {
    public init() {}

    @Dependency(\.continuousClock) var clock
    private enum CancelID { case loadDeadline }
    private static let log = Logger(subsystem: "dev.davidcollado.alondra", category: "Triage")

    @ObservableState
    public struct State: Equatable, Identifiable {
        public let id: String
        public var screen: ScreenFeature.State
        public var actions: ActionFeature.State
        public var attachments = AttachmentFeature.State()
        public var replyDraft = ""
        public var composition: Composition?
        public var awaitingAttachments = false

        public init(id: String, dataEpoch: String = "") {
            self.id = id
            screen = ScreenFeature.State(key: .card(id))
            actions = .device(dataEpoch: dataEpoch)
        }

        public var card: Card? {
            screen.document?.sections.lazy.flatMap(\.items).compactMap { item -> Card? in
                if case .card(let card) = item { card } else { nil }
            }.first
        }

        /// A card shows at most three buttons.
        public static let visibleLimit = 3

        /// The buttons under the card, in the order the daemon sends them.
        public var buttons: [WireAction] { card.map { Array($0.usableActions.prefix(Self.visibleLimit)) } ?? [] }

        /// What "More…" holds: the actions past the third, then the daemon's own extras.
        public var menu: [WireAction] { card.map { Array($0.usableActions.dropFirst(Self.visibleLimit)) + $0.usableMore } ?? [] }

        /// The field that writes to the node; every card has it.
        public var replyAction: WireAction? { card?.reply }

        public var allActions: [WireAction] { buttons + menu + (replyAction.map { [$0] } ?? []) }

        public var selected: WireAction? {
            guard let action = actions.action, allActions.contains(where: { $0.id == action.id }) else { return nil }
            return action
        }

        public var stamp: String? { selected == nil ? nil : actions.stamp }
    }

    /// A button that needs words or a reason before it can go, and the sheet that asks for them.
    public struct Composition: Equatable, Sendable, Identifiable {
        public let action: WireAction
        public var reason: String?
        public var note = ""

        public init(action: WireAction) { self.action = action }

        public var id: String { action.id ?? action.label }
        public var needsReason: Bool { !action.options.isEmpty }
        public var requiresNote: Bool {
            if case .text(_, let required, _) = action.input { return required }
            return false
        }
        public var canSend: Bool {
            (!needsReason || reason != nil) && (!requiresNote || !note.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    public enum Action {
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case attachments(AttachmentFeature.Action)
        case replyChanged(String)
        case menuOptionPicked(WireAction, optionID: String)
        case replySent(WireAction)
        case actionTapped(WireAction)
        case reasonChanged(String)
        case noteChanged(String)
        case opened(ViewKey)
        case loadTimedOut
        case compositionConfirmed
        case compositionCancelled
        case submit
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable {
            case open(ViewKey)
            /// The card was settled elsewhere: it leaves the queue, saying why.
            case settled(String)
        }
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Scope(state: \.attachments, action: \.attachments) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .screen(.loaded(let document)):
                guard document.key == state.screen.key.rawValue else {
                    let id = state.id
                    Self.log.error("card \(id, privacy: .public) loaded a view for \(document.key, privacy: .public)")
                    return .none
                }
                state.actions.dataEpoch = document.dataEpoch
                guard let why = document.settledNotice else { return .cancel(id: CancelID.loadDeadline) }
                return .merge(.cancel(id: CancelID.loadDeadline), .send(.delegate(.settled(why))))
            case .screen(.task), .screen(.retry):
                guard state.screen.document == nil else { return .none }
                return .run { [clock] send in
                    try await clock.sleep(for: TriageFeature.loadDeadline)
                    await send(.loadTimedOut)
                }
                .cancellable(id: CancelID.loadDeadline, cancelInFlight: true)
            case .screen(.stop):
                return .cancel(id: CancelID.loadDeadline)
            case .screen(.failed(let message)):
                let id = state.id
                Self.log.error("card \(id, privacy: .public) failed to load: \(message, privacy: .public)")
                return .cancel(id: CancelID.loadDeadline)
            case .screen(.missing):
                let id = state.id
                Self.log.error("card \(id, privacy: .public) has no view on the Mac")
                return .cancel(id: CancelID.loadDeadline)
            case .loadTimedOut:
                guard state.screen.document == nil, state.screen.phase == .loading else { return .none }
                let id = state.id
                Self.log.error("card \(id, privacy: .public) did not load within \(TriageFeature.loadDeadline.components.seconds) s")
                state.screen.phase = .failed(TriageFeature.loadFailure)
                return .none
            case .screen, .delegate:
                return .none
            case .opened(let key):
                return .send(.delegate(.open(key)))
            case .replyChanged(let text):
                state.replyDraft = text
                return .none
            case .menuOptionPicked(let action, let optionID):
                return .concatenate(
                    .send(.actions(.present(action))), .send(.actions(.chooseOption(optionID))), .send(.submit)
                )
            case .replySent(let action):
                return .concatenate(
                    state.actions.action == action ? .none : .send(.actions(.present(action))),
                    state.replyDraft.isEmpty ? .none : .send(.actions(.textChanged(state.replyDraft))),
                    .send(.submit)
                )
            case .actionTapped(let action):
                return tapped(action, state: &state)
            case .reasonChanged(let reason):
                state.composition?.reason = reason
                return .none
            case .noteChanged(let note):
                state.composition?.note = note
                return .none
            case .compositionCancelled:
                state.composition = nil
                return .none
            case .compositionConfirmed:
                guard let composition = state.composition, composition.canSend else { return .none }
                state.composition = nil
                return .concatenate(
                    .send(.actions(.present(composition.action))),
                    composition.reason.map { .send(.actions(.chooseOption($0))) } ?? .none,
                    composition.note.isEmpty ? .none : .send(.actions(.textChanged(composition.note))),
                    .send(.submit)
                )
            case .submit:
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
                return state.attachments.allReady ? .send(.submit) : .none
            case .actions(.response(let response)):
                guard response.outcome == .applied else { return .none }
                state.replyDraft = ""
                return .send(.attachments(.reset))
            case .actions:
                return .none
            }
        }
    }

    private func tapped(_ action: WireAction, state: inout State) -> Effect<Action> {
        guard action.enabled else { return .none }
        if action.comments != nil {
            if case .view(let key) = action.link { return .send(.opened(ViewKey(rawValue: key))) }
            return .send(.opened(.card(state.id)))
        }
        if let link = action.link {
            if case .view(let key) = link { return .send(.opened(ViewKey(rawValue: key))) }
            return .none
        }
        if Self.asksForWords(action) {
            state.composition = Composition(action: action)
            return .none
        }
        var effects: [Effect<Action>] = []
        if state.actions.action != action { effects.append(.send(.actions(.present(action)))) }
        if let words = action.carrying(state.replyDraft) {
            effects.append(.send(.actions(.textChanged(words))))
        }
        if action.presentation == .button { effects.append(.send(.submit)) }
        return .concatenate(effects)
    }

    /// A button that opens the sheet: it asks which reason applies, or for words of the person's own.
    /// An option never does: it answers when pressed, with what is written in the card's field.
    static func asksForWords(_ action: WireAction) -> Bool {
        if action.presentation == .options { return true }
        if action.optionId != nil { return false }
        if case .text = action.input { return true }
        return false
    }
}
