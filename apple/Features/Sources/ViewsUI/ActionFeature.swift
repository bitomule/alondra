import AlondraContract
import AlondraFeed
import AlondraAttachments
import ComposableArchitecture
import Foundation

public typealias WireAction = AlondraContract.Action

/// A screen embeds one instance for the action currently being presented. The server owns
/// eligibility and command mapping; this reducer owns only drafts, confirmation and delivery.
@Reducer
public struct ActionFeature: Sendable {
    @ObservableState
    public struct State: Equatable {
        public var action: WireAction?
        public var selectedOptionID: String?
        public var text = ""
        public var attachments: [String] = []
        public var comments: [ActionComment] = []
        public var fields: [String: JSONValue] = [:]
        public var confirmation: Confirm?
        public var pending: ActionRequest?
        public var inFlight = false
        public var stamp: String?
        public var error: String?
        public var changed: ActionConfirmation?
        public var attachmentPicker = AttachmentFeature.State()
        public var pendingAttachmentSubmit = false
        public var requiresAttachments = false
        public var handsOff = false
        public var dataEpoch: String
        public var client: ViewClient
        public var lang: String

        public init(dataEpoch: String, client: ViewClient, lang: String) {
            self.dataEpoch = dataEpoch; self.client = client; self.lang = lang
        }

        /// The confirm button, naming the option the action runs with: "Merge (squash)".
        public var confirmationButton: String? {
            guard let confirmation else { return nil }
            guard let id = selectedOptionID, let option = action?.options.first(where: { $0.id == id }) else {
                return confirmation.button
            }
            return "\(confirmation.button) (\(option.label))"
        }

        public var confirmationIsDestructive: Bool {
            action?.id?.hasPrefix("merge:") != true
        }

        /// Whether the send button can go now: the action is live and has what it asks for.
        public var canSubmit: Bool {
            guard let action, action.enabled, action.id != nil, !inFlight else { return false }
            return missingInput == nil
        }

        /// What the person still has to give before the action can go, as submit words it.
        var missingInput: String? {
            guard let action else { return nil }
            let typed = !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            let freeText: Bool = {
                guard selectedOptionID == nil, typed else { return false }
                if case .text = action.input { return true }
                return false
            }()
            if action.presentation == .options && !freeText && selectedOptionID == nil {
                return String(localized: "Choose an option")
            }
            let input = selectedOptionID.flatMap { id in action.options.first { $0.id == id }?.input } ?? action.input
            if case let .text(_, required, _) = input, required, comments.isEmpty, !typed, fields["answers"] == nil {
                return String(localized: "Enter a response")
            }
            return nil
        }
    }

    public enum Action {
        case present(WireAction)
        case presentForm(WireAction, [String: JSONValue], requiresAttachments: Bool)
        case chooseOption(String)
        case textChanged(String)
        case attachmentsChanged([String])
        case attachmentUploadFailed(String)
        case commentsChanged([ActionComment])
        case fieldsChanged([String: JSONValue])
        case attachmentPicker(AttachmentFeature.Action)
        case submit
        case confirm
        case cancelConfirmation
        case sendChangedAction
        /// The request left: the screen shows its result now, without waiting for the daemon.
        case sent(ActionRequest)
        /// The daemon answered `sent`, or could not be reached; a screen undoes what it showed when not applied.
        case settled(ActionRequest, applied: Bool)
        case response(ActionResponse)
        case failed(String)
        case dismiss
        case delegate(Delegate)

        /// What the screen showing this action reports upward; the app turns it into a toast.
        @CasePathable
        public enum Delegate: Equatable, Sendable {
            case completed(String)
            case failed(String)
        }
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.uuid) var uuid

    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.attachmentPicker, action: \.attachmentPicker) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case let .present(value):
                state.action = value
                state.requiresAttachments = value.attachments
                state.selectedOptionID = value.optionId; state.text = ""; state.attachments = []
                state.comments = []; state.fields = [:]; state.confirmation = nil
                state.pending = nil; state.inFlight = false; state.stamp = nil
                state.error = nil; state.changed = nil
                state.pendingAttachmentSubmit = false
                state.attachmentPicker = AttachmentFeature.State()
                return .none
            case let .presentForm(value, fields, requiresAttachments):
                state.action = value
                state.selectedOptionID = nil; state.text = ""; state.attachments = []
                state.comments = []; state.fields = fields; state.confirmation = nil
                state.pending = nil; state.inFlight = false; state.stamp = nil
                state.error = nil; state.changed = nil
                state.pendingAttachmentSubmit = false
                state.requiresAttachments = value.attachments || requiresAttachments
                state.attachmentPicker = AttachmentFeature.State()
                return .none
            case .attachmentPicker(let attachmentAction):
                if attachmentAction.invalidatesPendingSubmission { state.pendingAttachmentSubmit = false }
                return .none
            case .attachmentUploadFailed(let message):
                state.pendingAttachmentSubmit = false
                state.error = message
                return .none
            case let .chooseOption(id):
                guard state.action?.options.contains(where: { $0.id == id }) == true else { return .none }
                if state.selectedOptionID != id, !state.inFlight { state.pending = nil }
                state.selectedOptionID = id; state.error = nil
                return .none
            case let .textChanged(text):
                if state.text != text, !state.inFlight { state.pending = nil }
                state.text = text; state.error = nil
                return .none
            case let .attachmentsChanged(ids):
                state.attachments = ids
                guard state.pendingAttachmentSubmit, state.attachmentPicker.allReady else { return .none }
                state.pendingAttachmentSubmit = false
                return send(&state)
            case let .commentsChanged(comments): state.comments = comments; return .none
            case let .fieldsChanged(fields): state.fields = fields; return .none
            case .submit:
                guard let selected = state.action, selected.enabled, selected.id != nil, !state.inFlight else { return .none }
                if let missing = state.missingInput {
                    state.error = missing
                    return .none
                }
                if let confirm = selected.confirm {
                    state.confirmation = confirm
                    return .none
                }
                if state.requiresAttachments, !state.attachmentPicker.allReady {
                    state.pendingAttachmentSubmit = true
                    return .send(.attachmentPicker(.prepareForSubmission))
                }
                state.attachments = state.attachmentPicker.storedIDs
                return send(&state)
            case .confirm:
                guard state.confirmation != nil else { return .none }
                state.confirmation = nil
                if state.requiresAttachments, !state.attachmentPicker.allReady {
                    state.pendingAttachmentSubmit = true
                    return .send(.attachmentPicker(.prepareForSubmission))
                }
                state.attachments = state.attachmentPicker.storedIDs
                return send(&state)
            case .cancelConfirmation:
                state.confirmation = nil
                return .none
            case .sendChangedAction:
                guard let changed = state.changed else { return .none }
                state.changed = nil
                state.pending = makeRequest(state, actionID: changed.actionId)
                return send(&state)
            case .sent, .settled:
                return .none
            case let .response(reply):
                state.inFlight = false
                state.pending = nil
                switch reply.outcome {
                case .applied:
                    state.stamp = reply.stamp ?? state.stamp ?? state.action?.stamp
                    state.error = nil
                    return .send(.delegate(.completed(reply.message ?? String(localized: "Action completed"))))
                case .changed:
                    state.stamp = nil
                    state.changed = reply.confirm
                    state.error = reply.message
                    return .none
                case .noLongerAvailable, .rejected:
                    state.stamp = nil
                    state.error = reply.message
                    return .send(.delegate(.failed(reply.message ?? String(localized: "Action failed"))))
                }
            case let .failed(message):
                state.inFlight = false
                state.stamp = nil
                state.error = message
                // Keep the exact request and command ID for an explicit retry.
                return .send(.delegate(.failed(message)))
            case .delegate:
                return .none
            case .dismiss:
                state.action = nil; state.confirmation = nil; state.changed = nil
                state.error = nil; state.stamp = nil
                return .none
            }
        }.onChange(of: \.attachmentPicker.instanceID) { oldID, _ in
            Reduce { _, _ in AttachmentFeature.cancelEffects(instanceID: oldID).map { .attachmentPicker($0) } }
        }.onChange(of: \.attachmentPicker.storedIDs) { _, ids in
            Reduce { _, _ in .send(.attachmentsChanged(ids)) }
        }.onChange(of: \.attachmentPicker.failedUploadMessage) { _, message in
            if let message {
                Reduce { _, _ in .send(.attachmentUploadFailed(message)) }
            } else {
                Reduce { _, _ in .none }
            }
        }
    }

    /// The result shows at once: the stamp is up before the daemon answers, and the delivery runs on
    /// even if the screen goes away. A screen that leaves on sending hands the request off instead,
    /// to whoever outlives it.
    private func send(_ state: inout State) -> Effect<Action> {
        guard let action = state.action, let id = action.id else { return .none }
        let request = state.pending ?? makeRequest(state, actionID: id)
        state.stamp = action.stamp ?? String(localized: "Sent")
        state.error = nil
        guard !state.handsOff else {
            state.pending = nil
            return .send(.sent(request))
        }
        state.pending = request
        state.inFlight = true
        return .merge(.send(.sent(request)), .run { [viewsClient] send in
            let delivery = Task { try await viewsClient.perform(request) }
            do {
                let reply = try await delivery.value
                await send(.settled(request, applied: reply.outcome == .applied))
                await send(.response(reply))
            } catch {
                await send(.settled(request, applied: false))
                await send(.failed(String(localized: "Couldn’t send. Try again.")))
            }
        })
    }

    private func makeRequest(_ state: State, actionID: String) -> ActionRequest {
        ActionRequest(
            commandId: uuid().uuidString.lowercased(), dataEpoch: state.dataEpoch,
            client: state.client, lang: state.lang, actionId: actionID,
            optionId: state.selectedOptionID ?? (state.action?.presentation == .options && !state.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "" : nil),
            input: ActionInput(text: state.text.isEmpty ? nil : state.text,
                attachments: state.attachments.isEmpty ? nil : state.attachments,
                comments: state.comments.isEmpty ? nil : state.comments,
                fields: state.fields.isEmpty ? nil : state.fields)
        )
    }
}

public extension ActionFeature.State {
    /// An action state for this device: the iPhone client, in the language the server should answer in.
    static func device(dataEpoch: String = "") -> Self {
        let language = Locale.preferredLanguages.first?.split(separator: "-").first
        return Self(dataEpoch: dataEpoch, client: .ios, lang: language == "es" ? "es" : "en")
    }
}
