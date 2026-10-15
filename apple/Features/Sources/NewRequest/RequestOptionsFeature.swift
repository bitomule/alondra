import AlondraContract
import ComposableArchitecture
import Foundation

/// How a request is launched: which project, which issue, what happens to the pull requests and
/// how hard the node works. The composer owns the values; this sheet edits them and says what changed.
@Reducer
public struct RequestOptionsFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public let fields: [Field]
        public var values: [String: JSONValue]
        public var projectQuery = ""
        @Presents public var issuePicker: IssuePickerFeature.State?

        public init(fields: [Field], values: [String: JSONValue]) {
            self.fields = fields
            self.values = values
        }

        public func field(_ id: String) -> Field? { fields.first { $0.id == id } }

        public func choices(_ id: String) -> [ChoiceItem] {
            guard case .choice(let items)? = field(id)?.kind else { return [] }
            return items
        }

        public func selected(_ id: String) -> String {
            guard case .string(let value)? = values[id] ?? field(id)?.value else { return "" }
            return value
        }

        public func isOn(_ id: String) -> Bool {
            guard case .bool(let value)? = values[id] ?? field(id)?.value else { return false }
            return value
        }

        public var issue: String { selected("issue") }

        public var projectLabel: String? {
            let id = selected("project")
            return choices("project").first { $0.id == id && !id.isEmpty }?.label
        }

        public var visibleProjects: [ChoiceItem] {
            ProjectChoices.filter(choices("project"), query: projectQuery)
        }
    }

    public enum Action: BindableAction {
        case binding(BindingAction<State>)
        case choose(String, String)
        case toggle(String, Bool)
        case issueTapped
        case removeIssue
        case issuePicker(PresentationAction<IssuePickerFeature.Action>)
        case doneTapped
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable { case set(String, JSONValue) }
    }

    @Dependency(\.dismiss) var dismiss

    public var body: some ReducerOf<Self> {
        BindingReducer()
        Reduce { state, action in
            switch action {
            case .choose(let id, let value):
                state.values[id] = .string(value)
                return .send(.delegate(.set(id, .string(value))))
            case .toggle(let id, let enabled):
                state.values[id] = .bool(enabled)
                return .send(.delegate(.set(id, .bool(enabled))))
            case .issueTapped:
                state.issuePicker = IssuePickerFeature.State(selected: state.issue)
                return .none
            case .removeIssue:
                state.values["issue"] = .string("")
                return .send(.delegate(.set("issue", .string(""))))
            case .issuePicker(.presented(.delegate(.picked(let reference)))):
                state.issuePicker = nil
                state.values["issue"] = .string(reference)
                return .send(.delegate(.set("issue", .string(reference))))
            case .doneTapped:
                return .run { [dismiss] _ in await dismiss() }
            case .binding, .issuePicker, .delegate:
                return .none
            }
        }
        .ifLet(\.$issuePicker, action: \.issuePicker) { IssuePickerFeature() }
    }
}

public enum ProjectChoices {
    /// The projects whose name contains every word typed, in the order the daemon sent them.
    public static func filter(_ items: [ChoiceItem], query: String) -> [ChoiceItem] {
        let words = query.lowercased().split(whereSeparator: \.isWhitespace)
        guard !words.isEmpty else { return items }
        return items.filter { item in
            !item.id.isEmpty && words.allSatisfy { item.label.lowercased().contains($0) }
        }
    }
}
