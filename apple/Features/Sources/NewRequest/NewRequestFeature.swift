import AlondraContract
import ComposableArchitecture
import Foundation

/// The composer on the New tab: what you want done, what to attach, how to launch it, and the
/// requests already sent. Sending clears the composer at once; a request the daemon does not take
/// puts its text back.
@Reducer
public struct NewRequestFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen = ScreenFeature.State(key: .newRequest())
        public var actions = ActionFeature.State.ios()
        public var values: [String: JSONValue] = [:]
        @Presents public var options: RequestOptionsFeature.State?
        var formPresented = false

        public init() {}

        public var form: AlondraContract.Form? {
            screen.document?.sections.lazy.flatMap(\.items).compactMap { item -> AlondraContract.Form? in
                if case .form(let form) = item { form } else { nil }
            }.first
        }


        public var text: String {
            guard case .string(let text)? = values["text"] else { return "" }
            return text
        }

        public var canSend: Bool {
            form?.submit.enabled == true
                && !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
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
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case delegate(ScreenDelegate)
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .screen(.loaded(let document)):
                state.actions.dataEpoch = document.dataEpoch
                guard let form = state.form else { return .none }
                for field in form.fields where state.values[field.id] == nil { state.values[field.id] = field.value }
                guard state.formPresented else {
                    state.formPresented = true
                    return .send(.actions(.presentForm(form.submit, state.values, requiresAttachments: true)))
                }
                state.actions.action = form.submit
                return .none
            case .textChanged(let text):
                return Self.set("text", .string(text), in: &state)
            case .set(let id, let value):
                return Self.set(id, value, in: &state)
            case .optionsTapped:
                state.options = RequestOptionsFeature.State(fields: state.optionFields, values: state.values)
                return .none
            case .options(.presented(.delegate(.set(let id, let value)))):
                return Self.set(id, value, in: &state)
            case .send:
                guard state.canSend else { return .none }
                return .send(.actions(.submit))
            case .actions(.sent):
                state.values["text"] = .string("")
                guard let form = state.form else { return .none }
                return .send(.actions(.presentForm(form.submit, state.values, requiresAttachments: true)))
            case .actions(.settled(let request, applied: false)):
                guard state.text.isEmpty, let text = request.input.fields?["text"] else { return .none }
                return Self.set("text", text, in: &state)
            case .actions(.delegate(.completed(let message))):
                return .send(.delegate(.completed(message)))
            case .actions(.delegate(.failed(let message))):
                return .send(.delegate(.failed(message)))
            case .screen, .options, .actions, .delegate:
                return .none
            }
        }
        .ifLet(\.$options, action: \.options) { RequestOptionsFeature() }
    }

    private static func set(_ id: String, _ value: JSONValue, in state: inout State) -> Effect<Action> {
        state.values[id] = value
        state.actions.fields = state.values
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
