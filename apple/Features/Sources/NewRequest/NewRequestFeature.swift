import AlondraAttachments
import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// The composer for a new request, on the iPhone's New tab and in the Mac's window: what you want
/// done and what to attach are this device's; the launch options come from the daemon. Sending
/// clears the composer at once and puts the request on top of the queue, where it stays until the
/// daemon's queue shows it. Each request goes on its own, retrying by itself; one the daemon
/// refuses stays in the queue with its reason, to send again or discard.
@Reducer
public struct NewRequestFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen = ScreenFeature.State(key: .newRequest())
        public var values: [String: JSONValue] = [:]
        public var attachments = AttachmentFeature.State()
        public var pending = PendingRequests()
        public var outbox: IdentifiedArrayOf<OutgoingRequest.State> = []
        @Presents public var options: RequestOptionsFeature.State?
        public let client: ViewClient
        public let lang: String

        public init(client: ViewClient = .ios) {
            self.client = client
            let preferred = Locale.preferredLanguages.first?.split(separator: "-").first
            self.lang = preferred == "es" ? "es" : "en"
        }

        public var form: AlondraContract.Form? {
            screen.document?.sections.lazy.flatMap(\.items).compactMap { item -> AlondraContract.Form? in
                if case .form(let form) = item { form } else { nil }
            }.first
        }

        public var text: String {
            guard case .string(let text)? = values["text"] else { return "" }
            return text
        }

        /// Sending never waits for the daemon: only for something to say and attachments that can go.
        public var canSend: Bool {
            !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && attachments.canPrepareSubmission && attachments.failedUploadMessage == nil
        }

        public var optionFields: [Field] {
            form?.fields.filter { $0.id != "text" && $0.id != "attachments" } ?? []
        }

        /// The project the request goes to; `nil` leaves the choice to Alondra.
        public var project: String? {
            guard case .string(let id)? = values["project"], !id.isEmpty else { return nil }
            return choiceLabel("project", id: id)
        }

        /// The issue and the choices that differ from the defaults, one short phrase each.
        public var optionsSummary: [String] {
            ["issue", "pr", "effort"].compactMap { id in
                guard case .string(let value)? = values[id], !value.isEmpty else { return nil }
                return choiceLabel(id, id: value) ?? value
            }
        }

        public var recent: [AlondraContract.Row] {
            screen.document?.sections.first { $0.id == "sent" }?.items.compactMap { item in
                if case .row(let row) = item { row } else { nil }
            } ?? []
        }

        /// What this device sent that the daemon's queue does not show yet, newest first.
        public var unshown: [PendingRequests.Entry] { pending.unshown(recent) }

        public var recentTitle: String? { screen.document?.sections.first { $0.id == "sent" }?.title }

        private func choiceLabel(_ field: String, id: String) -> String? {
            guard case .choice(let items)? = form?.fields.first(where: { $0.id == field })?.kind else { return nil }
            return items.first { $0.id == id }?.label
        }
    }

    public enum Action {
        case task
        case textChanged(String)
        case set(String, JSONValue)
        case optionsTapped
        case options(PresentationAction<RequestOptionsFeature.Action>)
        case send
        case retry(String)
        case discard(String)
        case answered(String, ActionResponse)
        case undelivered(String, SendFailure)
        case expired(String)
        case screen(ScreenFeature.Action)
        case attachments(AttachmentFeature.Action)
        case outbox(IdentifiedActionOf<OutgoingRequest>)
        case delegate(ScreenDelegate)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    @Dependency(\.uuid) var uuid

    private enum CancelID: Hashable { case delivery(String), confirmation(String) }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.attachments, action: \.attachments) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .screen(.loaded(let document)):
                if let form = state.form {
                    for field in form.fields where state.values[field.id] == nil { state.values[field.id] = field.value }
                }
                state.pending.reconcile(shown: Set(state.recent.map(\.id)), version: document.version)
                let pending = state.pending
                state.outbox.removeAll { pending[$0.id] == nil }
                return .merge(state.pending.entries.filter { $0.phase == .waiting(.options) }.map { dispatch($0.id, &state) })
            case .textChanged(let text):
                return Self.set("text", .string(text), in: &state)
            case .set(let id, let value):
                return Self.set(id, value, in: &state)
            case .optionsTapped:
                guard !state.optionFields.isEmpty else { return .none }
                state.options = RequestOptionsFeature.State(fields: state.optionFields, values: state.values)
                return .none
            case .options(.presented(.delegate(.set(let id, let value)))):
                return Self.set(id, value, in: &state)
            case .send:
                guard state.canSend else { return .none }
                let id = uuid().uuidString.lowercased()
                let text = state.text.trimmingCharacters(in: .whitespacesAndNewlines)
                var fields = state.values
                fields["text"] = .string(text)
                state.values["text"] = .string("")
                let taken = state.attachments
                state.attachments = AttachmentFeature.State()
                guard taken.allReady else {
                    state.outbox.append(OutgoingRequest.State(id: id, fields: fields, attachments: Self.resumable(taken)))
                    state.pending.add(id, text: text, phase: .waiting(.attachments))
                    return .merge(
                        AttachmentFeature.cancelEffects(instanceID: taken.instanceID).map(Action.attachments),
                        .send(.outbox(.element(id: id, action: .attachments(.uploadNext))))
                    )
                }
                state.outbox.append(OutgoingRequest.State(id: id, fields: fields, attachments: taken))
                state.pending.add(id, text: text, phase: .sending)
                return dispatch(id, &state)
            case .outbox(.element(id: let id, action: .attachments)):
                guard state.pending[id]?.phase == .waiting(.attachments), state.outbox[id: id]?.attachments.allReady == true else {
                    return .none
                }
                return dispatch(id, &state)
            case .retry(let id):
                guard state.pending[id]?.isFailed == true else { return .none }
                return dispatch(id, &state)
            case .discard(let id):
                state.pending.discard(id)
                let attachments = state.outbox.remove(id: id)?.attachments.instanceID
                return .merge(
                    .cancel(id: CancelID.delivery(id)), .cancel(id: CancelID.confirmation(id)),
                    attachments.map { AttachmentFeature.cancelEffects(instanceID: $0).map(Action.attachments) } ?? .none
                )
            case .answered(let id, let reply):
                guard state.pending[id] != nil else { return .cancel(id: CancelID.confirmation(id)) }
                switch OutcomeStyle(reply) {
                case .success:
                    state.pending.set(id, .applied(shown: state.screen.document?.version))
                    state.outbox.remove(id: id)
                case .failure(let message):
                    state.pending.set(id, .failed(message))
                case .ask(let changed):
                    state.pending.set(id, .failed(changed.title))
                }
                return .cancel(id: CancelID.confirmation(id))
            case .undelivered(let id, let failure):
                state.pending.set(id, .failed(failure.message))
                return .cancel(id: CancelID.confirmation(id))
            case .expired(let id):
                state.pending.expire(id)
                return .none
            case .screen, .options, .attachments, .outbox, .delegate:
                return .none
            }
        }
        .ifLet(\.$options, action: \.options) { RequestOptionsFeature() }
        .forEach(\.outbox, action: \.outbox) { OutgoingRequest() }
    }

    /// Sends the request `id` now if it has what it needs, else leaves it waiting for it. The same
    /// command ID every time, so the daemon never takes it twice.
    private func dispatch(_ id: String, _ state: inout State) -> Effect<Action> {
        guard let outgoing = state.outbox[id: id] else { return .none }
        guard outgoing.attachments.allReady else {
            state.pending.set(id, .waiting(.attachments))
            return .none
        }
        guard let submit = state.form?.submit, let actionID = submit.id, let epoch = state.screen.document?.dataEpoch else {
            state.pending.set(id, .waiting(.options))
            return .none
        }
        let ids = outgoing.attachments.storedIDs
        let request = ActionRequest(
            commandId: id, dataEpoch: epoch, client: state.client, lang: state.lang, actionId: actionID,
            input: ActionInput(attachments: ids.isEmpty ? nil : ids, fields: outgoing.fields),
            resolvesCard: submit.resolvesCard
        )
        state.pending.set(id, .sending)
        return .merge(
            .run { [viewsClient] send in
                do {
                    let prepared = try await viewsClient.prepare(request)
                    try Task.checkCancellation()
                    do { await send(.answered(id, try await viewsClient.perform(prepared))) }
                    catch is CancellationError { return }
                    catch { await send(.undelivered(id, SendFailure(error))) }
                } catch is CancellationError {
                    return
                } catch {
                    await send(.undelivered(id, SendFailure(preparation: error)))
                }
            }
            .cancellable(id: CancelID.delivery(id), cancelInFlight: true),
            .run { [clock] send in
                try await clock.sleep(for: Optimistic.confirmationTimeout)
                await send(.expired(id))
            }
            .cancellable(id: CancelID.confirmation(id), cancelInFlight: true)
        )
    }

    /// Attachments that leave the composer before they finished uploading start again where the
    /// request now lives; what had already uploaded keeps its receipt.
    private static func resumable(_ attachments: AttachmentFeature.State) -> AttachmentFeature.State {
        var moved = attachments
        moved.instanceID = UUID()
        for index in moved.entries.indices where moved.entries[index].status == .uploading {
            moved.entries[index].status = .queued
            moved.entries[index].progress = 0
        }
        return moved
    }

    private static func set(_ id: String, _ value: JSONValue, in state: inout State) -> Effect<Action> {
        state.values[id] = value
        guard id == "project", case .string(let project) = value else { return .none }
        let key = ViewKey.newRequest(project: project.isEmpty ? nil : project)
        guard key != state.screen.key else { return .none }
        var reloading = ScreenFeature.State(key: key)
        reloading.document = state.screen.document
        reloading.phase = state.screen.phase
        state.screen = reloading
        return .send(.screen(.task))
    }
}
