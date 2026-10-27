import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// Loads one server-owned screen and keeps its feed registration scoped to this presentation.
@Reducer
public struct ViewsScreenFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var key: ViewKey
        public let owner: String
        public var document: ViewDocument?
        public var loading = false
        public var isPresented = false
        public var error: String?
        public var content: String?
        public var contentReference: ContentRef?
        public var form: WireForm?
        public var pickerFieldID: String?
        public var pickerDocument: ViewDocument?
        public var pickerQuery = ""
        public var pickerLoading = false
        public var pickerError: String?
        public var attachmentPreview: AttachmentPreview?
        public var action: ActionFeature.State

        public var hasInlineForm: Bool {
            ["new_request", "schedule:", "task_selection:", "model_profile:"].contains { key.rawValue.hasPrefix($0) }
        }

        /// The action's sheet steps aside while the server asks whether to send anyway, so the
        /// question is not left under the sheet, and comes back with what was being answered when
        /// the question is cancelled.
        public var actionSheetPresented: Bool {
            action.action != nil && action.changed == nil
        }

        public init(key: ViewKey, owner: String) {
            self.key = key
            self.owner = owner
            let preferred = Locale.preferredLanguages.first?.split(separator: "-").first
            self.action = ActionFeature.State(dataEpoch: "", client: .ios,
                                              lang: preferred == "es" ? "es" : "en")
        }
    }

    public enum Action {
        case appeared, disappeared, refresh
        case stale([String]), reset
        case loaded(ViewFetch), failed(String), missing
        case fetchContent(ContentRef), contentLoaded(String), contentFailed(String)
        case fetchAttachment(AlondraContract.Attachment), attachmentLoaded(AlondraContract.Attachment, Data), closeAttachment, dismissError
        case reloadForm(String, JSONValue)
        case presentForm(WireForm)
        case pickerOpened(String, String), pickerQueryChanged(String), pickerLoaded(ViewFetch), pickerFailed(String)
        case pickerSelected(String), pickerClosed
        case action(ActionFeature.Action)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    private enum CancelID { case load, picker }

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
                return load(state, force: true)
            case .disappeared:
                state.isPresented = false
                return .merge(.cancel(id: CancelID.load), .run { [viewsClient, owner = state.owner] _ in
                    await viewsClient.closeScreen(owner)
                })
            case .reloadForm(let fieldID, let value):
                guard fieldID == "project", case let .string(project) = value else { return .none }
                let next: ViewKey
                if state.key.rawValue.hasPrefix("new_request") { next = .newRequest(project: project) }
                else if state.key.rawValue.hasPrefix("schedule:new") { next = .newSchedule(project: project) }
                else { return .none }
                guard next != state.key else { return .none }
                state.key = next
                state.document = nil
                state.loading = true
                return load(state)
            case .presentForm(let form):
                state.form = form
                let fields = form.initialValues
                let attachments = form.fields.contains {
                    if case .attachments = $0.kind { true } else { false }
                }
                return .send(.action(.presentForm(form.submit, fields, requiresAttachments: attachments, modelFields: form.modelFields)))
            case .pickerOpened(let fieldID, let source):
                guard source == "issue_picker" else { return .none }
                state.pickerFieldID = fieldID
                state.pickerDocument = nil
                state.pickerQuery = ""
                state.pickerLoading = true
                state.pickerError = nil
                return loadPicker(query: "")
            case .pickerQueryChanged(let query):
                guard state.pickerFieldID != nil else { return .none }
                state.pickerQuery = query
                state.pickerLoading = true
                return .run { [clock, viewsClient] send in
                    do {
                        try await clock.sleep(for: .milliseconds(250))
                        await send(.pickerLoaded(try await viewsClient.load(.issuePicker(query: query), nil)))
                    } catch is CancellationError {
                        return
                    } catch {
                        await send(.pickerFailed(String(localized: "Couldn’t load this from your Mac. Try again.")))
                    }
                }.cancellable(id: CancelID.picker, cancelInFlight: true)
            case .pickerLoaded(.document(let document)):
                guard state.pickerFieldID != nil else { return .none }
                state.pickerDocument = document
                state.pickerLoading = false
                state.pickerError = nil
                return .none
            case .pickerLoaded(.unchanged):
                state.pickerLoading = false
                return .none
            case .pickerFailed(let message):
                state.pickerLoading = false
                state.pickerError = message
                return .none
            case .pickerSelected(let value):
                guard let fieldID = state.pickerFieldID else { return .none }
                state.action.fields[fieldID] = .string(value)
                state.pickerFieldID = nil
                state.pickerDocument = nil
                return .cancel(id: CancelID.picker)
            case .pickerClosed:
                state.pickerFieldID = nil
                state.pickerDocument = nil
                return .cancel(id: CancelID.picker)
            case .stale(let keys):
                guard state.isPresented else { return .none }
                guard keys.contains(state.key.rawValue) else { return .none }
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
                let sameEpoch = state.action.dataEpoch == document.dataEpoch
                state.action.dataEpoch = document.dataEpoch
                if let updated = document.sections.lazy.flatMap(\.items).compactMap({ item -> WireForm? in
                       if case let .form(form) = item { form } else { nil }
                   }).first, state.form != nil || state.hasInlineForm {
                    let wasEditing = state.form != nil
                    if let original = state.form, ["model_profile", "task_selection"].contains(original.id), original.submit.id != updated.submit.id {
                        if !state.action.inFlight {
                            if !sameEpoch { state.action.modelFormAvailable = false }
                            else if let id = updated.submit.id {
                                state.action.formReplacement = ActionFeature.ChangedNotice(title: String(localized: "This selection changed"), body: String(localized: "Your draft is retained. Save these choices against the current selection?"), actionId: id, button: String(localized: "Save retained choices"))
                            }
                        }
                        state.loading = false; return .none
                    }
                    state.form = updated
                    state.action.action = updated.submit
                    let edited = state.action.fields["substitution_was_edited"]
                    state.action.modelFields = updated.modelFields; state.action.modelFormAvailable = true
                    state.action.fields = Dictionary(uniqueKeysWithValues: updated.fields.map {
                        ($0.id, wasEditing ? state.action.fields[$0.id] ?? $0.value : $0.value)
                    })
                    if let edited = edited ?? updated.initialValues["substitution_was_edited"] { state.action.fields["substitution_was_edited"] = edited }
                    state.action.requiresAttachments = updated.submit.attachments || updated.fields.contains {
                        if case .attachments = $0.kind { true } else { false }
                    }
                }
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
            case .missing:
                if !state.action.modelFields.isEmpty { state.action.modelFormAvailable = false }
                state.loading = false
                state.document = nil
                state.content = nil
                state.contentReference = nil
                state.error = String(localized: "This screen is no longer available")
                return .none
            case .fetchContent(let reference):
                state.content = nil
                state.contentReference = reference
                return .run { [viewsClient] send in
                    do { await send(.contentLoaded(try await viewsClient.fetchContent(reference))) }
                    catch { await send(.contentFailed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
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
                    catch { await send(.contentFailed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
                }
            case .attachmentLoaded(let attachment, let data):
                state.attachmentPreview = AttachmentPreview(attachment: attachment, data: data)
                return .none
            case .closeAttachment:
                state.attachmentPreview = nil
                return .none
            case .dismissError:
                state.error = nil
                return .none
            case .action(.response(let response)) where response.outcome == .applied:
                state.loading = true
                if state.hasInlineForm {
                    state.form = nil
                    state.action.action = nil
                    return load(state, force: true)
                }
                return .merge(load(state), .send(.action(.dismiss)))
            case .action(.present):
                state.form = nil
                return .none
            case .action(.dismiss):
                state.form = nil
                return .none
            case .action:
                return .none
            }
        }
    }

    private func load(_ state: State, force: Bool = false) -> Effect<Action> {
        let key = state.key
        let owner = state.owner
        let version = force ? nil : state.document?.version
        return .run { [viewsClient] send in
            await viewsClient.openScreen(owner, [key])
            do { await send(.loaded(try await viewsClient.load(key, version))) }
            catch ViewsClientError.missingView { await send(.missing) }
            catch { await send(.failed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
        }.cancellable(id: CancelID.load, cancelInFlight: true)
    }

    private func loadPicker(query: String) -> Effect<Action> {
        .run { [viewsClient] send in
            do { await send(.pickerLoaded(try await viewsClient.load(.issuePicker(query: query), nil))) }
            catch { await send(.pickerFailed(String(localized: "Couldn’t load this from your Mac. Try again."))) }
        }.cancellable(id: CancelID.picker, cancelInFlight: true)
    }
}

public struct AttachmentPreview: Equatable, Identifiable {
    public let attachment: AlondraContract.Attachment
    public let data: Data
    public var id: String { attachment.id }
}
