import AlondraContract
import ComposableArchitecture
import Foundation

/// The form that creates or edits a scheduled task: what to do, when to repeat it, how to launch it.
/// Typing, picking and validation errors live here; the daemon decides whether the schedule is
/// valid and names the field it refuses.
@Reducer
public struct ScheduleEditorFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen: ScreenFeature.State
        public var actions = ActionFeature.State.ios()
        public var values: [String: JSONValue] = [:]
        public var errors: [String: String] = [:]
        public var formError: String?
        var seeded = false
        var rejected = false
        var loadedProject: String?

        public init(key: ViewKey) { screen = ScreenFeature.State(key: key) }

        public var isNew: Bool { screen.key.rawValue.hasPrefix("schedule:new") }
        public var title: String? { screen.document?.title }
        public var notices: [Notice] { screen.document?.notices ?? [] }

        public var form: AlondraContract.Form? {
            screen.document?.sections.lazy.flatMap(\.items).compactMap { item -> AlondraContract.Form? in
                if case .form(let form) = item { form } else { nil }
            }.first
        }

        public func field(_ id: String) -> Field? { form?.fields.first { $0.id == id } }

        public func value(_ id: String) -> JSONValue { values[id] ?? field(id)?.value ?? .null }

        public func text(_ id: String) -> String {
            if case .string(let text) = value(id) { text } else { "" }
        }

        public func isVisible(_ field: Field) -> Bool {
            form.map { ScheduleEditorRules.isVisible(field, values: values, in: $0) } ?? false
        }

        /// Saving waits for the daemon, which may refuse a field; the form shows it is saving meanwhile.
        public var isSaving: Bool { actions.inFlight }

        public var canSave: Bool {
            form != nil && !actions.inFlight
                && !text("name").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && !text("mission").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    public enum Action {
        case task
        case set(String, JSONValue)
        case save
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case delegate(ScreenDelegate)
    }

    @Dependency(\.dismiss) var dismiss
    @Dependency(\.date.now) var now

    private static let startsOnADate: Set<String> = ["every_hours", "every_days", "every_months", "once"]

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
                let project = Self.project(in: form)
                let launchOptions = Set(ScheduleEditorRules.launchOptionFields(in: form).map(\.id))
                let reloadsOptions = state.seeded && project != state.loadedProject
                for field in form.fields where state.values[field.id] == nil || (reloadsOptions && launchOptions.contains(field.id)) {
                    state.values[field.id] = field.value
                }
                state.seeded = true
                state.loadedProject = project
                return .none
            case .set(let id, let value):
                state.values[id] = value
                state.errors[id] = nil
                state.formError = nil
                if id == "repeat", case .string(let frequency) = value, Self.startsOnADate.contains(frequency),
                   case .null = state.value("start_at") {
                    state.values["start_at"] = .integer(Int64((now.timeIntervalSince1970 * 1000).rounded()))
                }
                guard id == "project", state.isNew, case .string(let project) = value else { return .none }
                var reloading = ScreenFeature.State(key: .newSchedule(project: project.isEmpty ? nil : project))
                reloading.document = state.screen.document
                reloading.phase = state.screen.phase
                state.screen = reloading
                return .send(.screen(.task))
            case .save:
                guard state.canSave, let form = state.form else { return .none }
                state.errors = [:]
                state.formError = nil
                state.rejected = false
                return .concatenate(
                    .send(.actions(.presentForm(form.submit, state.values, requiresAttachments: false))),
                    .send(.actions(.submit))
                )
            case .actions(.response(let reply)):
                guard reply.outcome == .rejected else { return .none }
                state.rejected = true
                if let field = reply.field, let message = reply.message {
                    state.errors[field] = message
                } else {
                    state.formError = reply.message
                }
                return .none
            case .actions(.delegate(.completed(let message))):
                return .concatenate(
                    .send(.delegate(.completed(message))),
                    .run { [dismiss] _ in await dismiss() }
                )
            case .actions(.delegate(.failed(let message))):
                guard !state.rejected else {
                    state.rejected = false
                    return .none
                }
                return .send(.delegate(.failed(message)))
            case .screen, .actions, .delegate:
                return .none
            }
        }
    }

    private static func project(in form: AlondraContract.Form) -> String? {
        guard case .string(let project)? = form.fields.first(where: { $0.id == "project" })?.value else { return nil }
        return project
    }
}
