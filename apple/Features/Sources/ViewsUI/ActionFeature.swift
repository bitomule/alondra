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
        public var modelFields: [Field] = []
        public var modelFormAvailable = true
        /// The MCP form the card asks to fill and its values, sent only with the option that accepts it.
        public var requestedForm: RequestedForm?
        public var content: [String: JSONValue] = [:]
        public var formReplacement: ChangedNotice?
        public var confirmation: Confirm?
        public var pending: ActionRequest?
        public var inFlight = false
        public var stamp: String?
        /// How many times the person has sent an action from here. The screen plays its haptic on
        /// each send, the moment it is tapped, without waiting for the Mac.
        public var sends = 0
        public var error: String?
        public var changed: ChangedNotice?
        /// The last request that went out and got no answer. Sending the same again reuses its
        /// command, so the Mac does it once even if the first one did arrive.
        public var unsure: ActionRequest?
        public var attachmentPicker = AttachmentFeature.State()
        public var pendingAttachmentSubmit = false
        public var requiresAttachments = false
        public var handsOff = false
        /// The Mac has not answered for a while; the send goes on retrying on its own.
        public var stalled = false
        public var dataEpoch: String {
            didSet {
                guard dataEpoch != oldValue else { return }
                pending = nil; inFlight = false; unsure = nil; stalled = false
                stamp = nil; error = nil; confirmation = nil; changed = nil; formReplacement = nil
                if !modelFields.isEmpty { modelFormAvailable = false }
                pendingAttachmentSubmit = false
            }
        }
        public var client: ViewClient
        public var lang: String

        public init(dataEpoch: String, client: ViewClient, lang: String) {
            self.dataEpoch = dataEpoch; self.client = client; self.lang = lang
        }

        /// Only an action the server styles destructive asks in red: merging is a question, closing a PR is not.
        public var confirmationIsDestructive: Bool {
            action?.style == .destructive
        }

        /// Whether the send button can go now: the action is live and has what it asks for.
        public var canSubmit: Bool {
            guard let action, action.enabled, action.id != nil, !inFlight else { return false }
            return missingInput == nil && modelUnavailableReason == nil
        }

        public var modelUnavailableReason: String? {
            guard modelFormAvailable else { return String(localized: "Unavailable in the current catalog") }
            guard !modelFields.isEmpty else { return nil }
            let options = RequestOptionsFeature.State(fields: modelFields, values: fields)
            for field in options.visibleFields {
                guard case .choice = field.kind else { continue }
                let selected = options.selected(field.id)
                guard let item = options.choices(field.id).first(where: { $0.id == selected }) else { return String(localized: "Unavailable in the current catalog") }
                if let reason = options.unavailableReason(item) { return reason }
            }
            return nil
        }

        /// What the card says while a send waits on a Mac that does not answer.
        public var stallNotice: String? {
            guard stalled, inFlight else { return nil }
            return stamp == nil ? String(localized: "Couldn’t send · Retrying…")
                : String(localized: "Your Mac hasn’t confirmed it yet · Retrying…")
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
            if let requestedForm, selectedOptionID == RequestedForm.acceptOption, let problem = requestedForm.problem(content) {
                return problem
            }
            let input = selectedOptionID.flatMap { id in action.options.first { $0.id == id }?.input } ?? action.input
            if case let .text(_, required, _, _) = input, required, action.optionId == nil, comments.isEmpty, !typed {
                return String(localized: "Enter a response")
            }
            return nil
        }
    }

    /// What the server says moved while the person looked, and the action that goes on anyway.
    public struct ChangedNotice: Equatable, Sendable {
        public let title: String
        public let body: String?
        public let actionId: String
        public let button: String
        public var resolvesCard: Bool? = nil
        public init(title: String, body: String?, actionId: String, button: String, resolvesCard: Bool? = nil) { self.title = title; self.body = body; self.actionId = actionId; self.button = button; self.resolvesCard = resolvesCard }
    }

    public enum Action {
        case present(WireAction)
        case presentForm(WireAction, [String: JSONValue], requiresAttachments: Bool, modelFields: [Field] = [])
        case chooseOption(String)
        case textChanged(String)
        case attachmentsChanged([String])
        case attachmentUploadFailed(String)
        case commentsChanged([ActionComment])
        case fieldsChanged([String: JSONValue])
        /// The card's MCP form and what the person filled in, to go with the option that accepts it.
        case formFilled(RequestedForm, [String: JSONValue])
        case attachmentPicker(AttachmentFeature.Action)
        case submit
        case confirm
        case cancelConfirmation
        case sendChangedAction
        /// The person sent `request`: a screen can show its result at once, before it is prepared.
        case sending(ActionRequest)
        /// The request left: the screen shows its result now, without waiting for the daemon.
        case prepared(ActionRequest, ActionRequest)
        case sent(ActionRequest)
        /// The daemon answered `sent`, or could not be reached; a screen undoes what it showed when not applied.
        case settled(ActionRequest, applied: Bool)
        case response(ActionResponse)
        /// The daemon answered `request`. Only the request this action is still waiting on changes
        /// what it shows; an answer to an earlier one is only reported.
        case answered(ActionRequest, ActionResponse)
        case failed(String)
        /// The request got no answer from the daemon, for the reason given.
        case undelivered(ActionRequest, SendFailure)
        case dismiss
        /// The person closed the notice that something changed; what they were answering stays open.
        case dismissChanged
        /// The send for this command got no answer in time: the card says so and keeps retrying.
        case stalled(String)
        /// The person asked to try again now instead of waiting for the next retry.
        case retryNow
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

    /// Names the delivery of one command, so a parent that gives up waiting can stop listening for its answer.
    public struct DeliveryID: Hashable, Sendable {
        public let commandID: String
        public init(commandID: String) { self.commandID = ActionRequest.deliveryNonce(commandID) }
    }

    private struct StallID: Hashable { let nonce: String }

    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.attachmentPicker, action: \.attachmentPicker) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case let .present(value):
                guard !(state.inFlight && state.stamp == nil) else { return .none }
                state.action = value
                state.requiresAttachments = value.attachments
                state.selectedOptionID = value.optionId; state.text = ""; state.attachments = []
                state.comments = []; state.fields = [:]; state.modelFields = []; state.modelFormAvailable = true; state.formReplacement = nil; state.confirmation = nil
                state.requestedForm = nil; state.content = [:]
                state.pending = nil; state.inFlight = false; state.stamp = nil
                state.error = nil; state.changed = nil; state.stalled = false
                state.pendingAttachmentSubmit = false
                state.attachmentPicker = AttachmentFeature.State()
                return .none
            case let .presentForm(value, fields, requiresAttachments, modelFields):
                guard !(state.inFlight && state.stamp == nil) else { return .none }
                state.action = value
                state.selectedOptionID = nil; state.text = ""; state.attachments = []
                state.comments = []; state.fields = fields; state.modelFields = modelFields; state.modelFormAvailable = true; state.formReplacement = nil; state.confirmation = nil
                state.requestedForm = nil; state.content = [:]
                state.pending = nil; state.inFlight = false; state.stamp = nil
                state.error = nil; state.changed = nil; state.stalled = false
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
            case let .formFilled(form, content):
                if state.content != content, !state.inFlight { state.pending = nil }
                state.requestedForm = form; state.content = content; state.error = nil
                return .none
            case .fieldsChanged(var fields):
                if fields["model"] != state.fields["model"] {
                    if fields["reasoning_effort"] != nil { fields["reasoning_effort"] = .string("auto") }
                    if fields["allow_model_substitution"] != nil, state.fields["substitution_was_edited"] != .bool(true) { fields["allow_model_substitution"] = .bool(fields["model"] == .string("auto")); fields["substitution_was_edited"] = .bool(false) }
                } else if fields["allow_model_substitution"] != state.fields["allow_model_substitution"], fields["allow_model_substitution"] != nil { fields["substitution_was_edited"] = .bool(true) }
                for id in Array(fields.keys) where id.hasPrefix("candidate_") && fields[id] != state.fields[id] { fields["reasoning_effort_" + String(id.dropFirst("candidate_".count))] = .string("auto") }
                if fields["pin_account"] != state.fields["pin_account"], fields["pin_account"] == .bool(true), fields["account"] != .string("auto") { fields["account_was_edited"] = .bool(true) }
                if fields["account"] != state.fields["account"], fields["pin_account"] != nil { fields["pin_account"] = .bool(fields["account"] != .string("auto")); fields["account_was_edited"] = .bool(true) }
                state.fields = fields; return .none
            case .submit:
                if state.inFlight, state.stalled { return .send(.retryNow) }
                guard let selected = state.action, selected.enabled, selected.id != nil, !state.inFlight else { return .none }
                if let reason = state.modelUnavailableReason { state.error = reason; return .none }
                if let replacement = state.formReplacement { state.changed = replacement; return .none }
                if let missing = state.missingInput {
                    state.error = missing
                    return .none
                }
                if let confirm = selected.confirm {
                    state.confirmation = confirm
                    return .none
                }
                state.sends += 1
                if state.requiresAttachments, !state.attachmentPicker.allReady {
                    state.pendingAttachmentSubmit = true
                    return .send(.attachmentPicker(.prepareForSubmission))
                }
                state.attachments = state.attachmentPicker.storedIDs
                return send(&state)
            case .confirm:
                guard state.confirmation != nil else { return .none }
                state.confirmation = nil
                state.sends += 1
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
                if let reason = state.modelUnavailableReason { state.changed = nil; state.error = reason; return .none }
                state.changed = nil; state.formReplacement = nil
                state.sends += 1
                state.pending = makeRequest(state, actionID: changed.actionId, resolvesCard: changed.resolvesCard)
                return send(&state)
            case let .prepared(draft, request):
                guard let action = state.action else { return .none }
                guard draft.dataEpoch == state.dataEpoch, request.dataEpoch == state.dataEpoch,
                      state.pending == draft else { return .none }
                state.pending = request
                state.stamp = action.stamp ?? String(localized: "Sent")
                return deliver(request)
            case .sending, .sent, .settled:
                return .none
            case let .answered(request, reply):
                guard request.dataEpoch == state.dataEpoch else { return .none }
                guard state.pending?.commandId == request.commandId else {
                    switch OutcomeStyle(reply) {
                    case .success(let message): return .send(.delegate(.completed(message)))
                    case .failure(let message): return .send(.delegate(.failed(message)))
                    case .ask(let changed): return .send(.delegate(.failed(changed.title)))
                    }
                }
                return .send(.response(reply))
            case let .response(reply):
                let stall = stopStall(&state)
                state.inFlight = false
                state.pending = nil
                state.unsure = nil
                return .merge(stall, respond(reply, &state))
            case let .failed(message):
                state.inFlight = false
                state.stamp = nil
                state.error = message
                // Keep the exact request and command ID for an explicit retry.
                return .send(.delegate(.failed(message)))
            case let .undelivered(request, failure):
                guard request.dataEpoch == state.dataEpoch else { return .none }
                guard state.pending?.commandId == request.commandId else {
                    return .send(.delegate(.failed(failure.message)))
                }
                let stall = stopStall(&state)
                state.inFlight = false
                state.pending = nil
                state.stamp = nil
                state.error = failure.message
                state.unsure = failure == .unsure ? request : nil
                return .merge(stall, .send(.delegate(.failed(failure.message))))
            case let .stalled(commandID):
                guard state.inFlight, let pending = state.pending,
                      ActionRequest.deliveryNonce(pending.commandId) == ActionRequest.deliveryNonce(commandID) else { return .none }
                state.stalled = true
                return .none
            case .retryNow:
                return .run { [viewsClient] _ in await viewsClient.retryNow() }
            case .delegate:
                return .none
            case .dismissChanged:
                state.changed = nil
                return .none
            case .dismiss:
                let stall = stopStall(&state)
                state.action = nil; state.confirmation = nil; state.changed = nil
                state.pending = nil; state.inFlight = false
                state.error = nil; state.stamp = nil
                return stall
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

    private func respond(_ reply: ActionResponse, _ state: inout State) -> Effect<Action> {
        switch OutcomeStyle(reply) {
        case .success(let message):
            state.stamp = reply.stamp ?? state.stamp ?? state.action?.stamp
            state.error = nil
            return .send(.delegate(.completed(message)))
        case .ask(let changed):
            state.stamp = nil
            state.changed = changed
            state.error = nil
            return .none
        case .failure(let message):
            state.stamp = nil
            state.error = message
            return .send(.delegate(.failed(message)))
        }
    }

    private func stopStall(_ state: inout State) -> Effect<Action> {
        state.stalled = false
        guard let pending = state.pending else { return .none }
        return .cancel(id: StallID(nonce: ActionRequest.deliveryNonce(pending.commandId)))
    }

    /// A send the Mac has not answered after a while says so on the card, and goes on retrying.
    private func stallTimer(_ request: ActionRequest) -> Effect<Action> {
        .run { [viewsClient] send in
            guard let delay = viewsClient.stallAfter() else { return }
            try await Task.sleep(for: delay)
            await send(.stalled(request.commandId))
        }
        .cancellable(id: StallID(nonce: ActionRequest.deliveryNonce(request.commandId)), cancelInFlight: true)
    }

    /// The stamp appears once the intent is prepared, before the daemon answers. Delivery runs on
    /// even if the screen goes away. A screen that leaves on sending hands the request off
    /// unprepared, to whoever outlives it.
    private func send(_ state: inout State) -> Effect<Action> {
        guard let action = state.action, let id = action.id else { return .none }
        let request = state.pending ?? makeRequest(state, actionID: id)
        state.error = nil
        if state.handsOff {
            state.stamp = action.stamp ?? String(localized: "Sent")
            state.pending = nil; state.inFlight = false
            return .concatenate(.send(.sending(request)), .send(.sent(request)))
        }
        state.stamp = nil
        state.pending = request
        state.inFlight = true
        state.stalled = false
        return .merge(.send(.sending(request)), stallTimer(request), .run { [viewsClient] send in
            do {
                let prepared = try await viewsClient.prepare(request)
                try Task.checkCancellation()
                await send(.prepared(request, prepared))
            } catch { await send(.undelivered(request, SendFailure(preparation: error))) }
        }
        .cancellable(id: DeliveryID(commandID: request.commandId)))
    }

    private func deliver(_ request: ActionRequest) -> Effect<Action> {
        .merge(.send(.sent(request)), .run { [viewsClient] send in
            let delivery = Task { try await viewsClient.perform(request) }
            do {
                let reply = try await delivery.value
                await send(.settled(request, applied: reply.outcome == .applied))
                await send(.answered(request, reply))
            } catch {
                await send(.settled(request, applied: false))
                await send(.undelivered(request, SendFailure(error)))
            }
        }
        .cancellable(id: DeliveryID(commandID: request.commandId)))
    }

    private func makeRequest(_ state: State, actionID: String, resolvesCard: Bool? = nil) -> ActionRequest {
        let request = ActionRequest(
            commandId: uuid().uuidString.lowercased(), dataEpoch: state.dataEpoch,
            client: state.client, lang: state.lang, actionId: actionID,
            optionId: state.selectedOptionID ?? (state.action?.presentation == .options && !state.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "" : nil),
            input: ActionInput(text: state.text.isEmpty ? nil : state.text,
                attachments: state.attachments.isEmpty ? nil : state.attachments,
                comments: state.comments.isEmpty ? nil : state.comments,
                fields: state.fields.isEmpty ? nil : state.fields,
                content: state.requestedForm != nil && state.selectedOptionID == RequestedForm.acceptOption ? state.content : nil),
            resolvesCard: resolvesCard ?? state.action?.resolvesCard
        )
        guard let unsure = state.unsure, unsure.dataEpoch == request.dataEpoch, unsure.actionId == request.actionId,
              unsure.optionId == request.optionId, unsure.input == request.input else { return request }
        return unsure
    }
}

public extension ActionFeature.State {
    /// An action state for this device: the iPhone client, in the language the server should answer in.
    static func device(dataEpoch: String = "") -> Self {
        let language = Locale.preferredLanguages.first?.split(separator: "-").first
        return Self(dataEpoch: dataEpoch, client: .ios, lang: language == "es" ? "es" : "en")
    }
}
