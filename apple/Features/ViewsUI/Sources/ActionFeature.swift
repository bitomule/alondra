import AlondraViews
import AlondraViewsDependency
import AlondraAttachmentsFeature
import ComposableArchitecture
import Foundation

public typealias WireAction = AlondraViews.Action

/// A screen embeds one instance for the action currently being presented. The server owns
/// eligibility and command mapping; this reducer owns only drafts, confirmation and delivery.
@Reducer
public struct ActionFeature {
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
        public var dataEpoch: String
        public var client: ViewClient
        public var lang: String

        public init(dataEpoch: String, client: ViewClient, lang: String) {
            self.dataEpoch = dataEpoch; self.client = client; self.lang = lang
        }

        public var hiddenCardID: String? {
            guard inFlight, let id = action?.id else { return nil }
            let subject = id.split(separator: "@").first?.split(separator: ":").last
            return subject.map(String.init)
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
        case response(ActionResponse)
        case failed(String)
        case dismiss
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
                state.selectedOptionID = nil; state.text = ""; state.attachments = []
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
                state.selectedOptionID = id; state.error = nil
                return .none
            case let .textChanged(text): state.text = text; state.error = nil; return .none
            case let .attachmentsChanged(ids):
                state.attachments = ids
                guard state.pendingAttachmentSubmit, state.attachmentPicker.allReady else { return .none }
                state.pendingAttachmentSubmit = false
                return send(&state)
            case let .commentsChanged(comments): state.comments = comments; return .none
            case let .fieldsChanged(fields): state.fields = fields; return .none
            case .submit:
                guard let selected = state.action, selected.enabled, selected.id != nil, !state.inFlight else { return .none }
                let freeText: Bool = {
                    guard state.selectedOptionID == nil,
                          !state.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
                    if case .text = selected.input { return true }
                    return false
                }()
                if selected.presentation == .options && !freeText && state.selectedOptionID == nil {
                    state.error = String(localized: "Choose an option")
                    return .none
                }
                let input = state.selectedOptionID.flatMap { id in selected.options.first { $0.id == id }?.input } ?? selected.input
                if case let .text(_, required, _) = input, required && state.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    state.error = String(localized: "Enter a response")
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
            case let .response(reply):
                state.inFlight = false
                state.pending = nil
                switch reply.outcome {
                case .applied:
                    state.stamp = reply.stamp ?? state.action?.stamp
                    state.error = nil
                case .changed:
                    state.changed = reply.confirm
                    state.error = reply.message
                case .noLongerAvailable, .rejected:
                    state.error = reply.message
                }
                return .none
            case let .failed(message):
                state.inFlight = false
                state.error = message
                // Keep the exact request and command ID for an explicit retry.
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

    private func send(_ state: inout State) -> Effect<Action> {
        guard let id = state.action?.id else { return .none }
        let request = state.pending ?? makeRequest(state, actionID: id)
        state.pending = request
        state.inFlight = true
        state.error = nil
        return .run { send in
            do { await send(.response(try await viewsClient.perform(request))) }
            catch { await send(.failed(String(localized: "Couldn’t send. Try again."))) }
        }
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
