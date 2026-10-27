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
        public var fields: [Field]
        public var values: [String: JSONValue]
        public var projectQuery = ""
        public var choiceQuery = ""
        public var selectionLabels: [String: String] = [:]
        @Presents public var issuePicker: IssuePickerFeature.State?

        public init(fields: [Field], values: [String: JSONValue]) {
            self.fields = fields
            self.values = values
        }

        public mutating func refresh(fields next: [Field]) {
            for id in ["model", "account", "reasoning_effort"] {
                if choices(id).contains(where: { $0.id == selected(id) }) { selectionLabels[id] = selectedLabel(id) }
            }
            fields = next
        }

        public func field(_ id: String) -> Field? { fields.first { $0.id == id } }

        public func choices(_ id: String) -> [ChoiceItem] {
            guard case .choice(let items)? = field(id)?.kind else { return [] }
            return items
        }

        public var visibleFields: [Field] {
            fields.filter { field in
                guard let condition = field.visibleWhen else { return true }
                return condition.matches(values[condition.field] ?? self.field(condition.field)?.value ?? .null)
            }
        }

        public func isAvailable(_ item: ChoiceItem) -> Bool {
            item.disabledReason == nil && item.availableWhen.map {
                $0.matches(values[$0.field] ?? field($0.field)?.value ?? .null)
            } != false
        }

        public func unavailableReason(_ item: ChoiceItem) -> String? {
            if let reason = item.disabledReason { return reason }
            return isAvailable(item) ? nil : String(localized: "Unavailable for the selected model")
        }

        public func choicesIncludingSelection(_ id: String) -> [ChoiceItem] {
            var items = choices(id)
            let value = selected(id)
            if !value.isEmpty && !items.contains(where: { $0.id == value }) {
                items.append(ChoiceItem(id: value, label: selectionLabels[id] ?? value, group: String(localized: "Unavailable"), disabledReason: String(localized: "Unavailable in the current catalog")))
            }
            return items
        }

        public func choiceGroups(_ id: String) -> [RequestChoiceGroup] {
            let words = choiceQuery.lowercased().split(whereSeparator: \.isWhitespace)
            let items = choicesIncludingSelection(id).filter { item in
                let text = [item.label, item.group ?? "", item.description ?? ""].joined(separator: " ").lowercased()
                return words.allSatisfy(text.contains)
            }
            var groups: [RequestChoiceGroup] = []
            for item in items {
                let title = item.group ?? ""
                if let index = groups.firstIndex(where: { $0.title == title }) { groups[index].items.append(item) }
                else { groups.append(RequestChoiceGroup(title: title, items: [item])) }
            }
            return groups
        }

        public func selectedLabel(_ id: String) -> String {
            guard let item = choicesIncludingSelection(id).first(where: { $0.id == selected(id) }) else { return selected(id) }
            if ["model", "account"].contains(id), let group = item.group, group != String(localized: "Unavailable") { return "\(group) · \(item.label)" }
            return item.label
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
                guard let item = state.choices(id).first(where: { $0.id == value }), state.isAvailable(item) else { return .none }
                let old = state.selected(id)
                state.values[id] = .string(value)
                state.selectionLabels[id] = item.label
                var effects: [Effect<Action>] = [.send(.delegate(.set(id, .string(value))))]
                if id == "account", old != value, state.field("pin_account") != nil {
                    state.values["pin_account"] = .bool(value != "auto")
                    effects.append(.send(.delegate(.set("pin_account", .bool(value != "auto")))))
                }
                if id == "model", old != value {
                    let effort = state.selected("reasoning_effort")
                    if effort != "auto", state.field("reasoning_effort") != nil {
                        state.values["reasoning_effort"] = .string("auto")
                        effects.append(.send(.delegate(.set("reasoning_effort", .string("auto")))))
                    }
                    if state.field("allow_model_substitution") != nil, state.values["substitution_was_edited"] != .bool(true) {
                        let automatic = value == "auto"
                        state.values["allow_model_substitution"] = .bool(automatic)
                        effects.append(.send(.delegate(.set("allow_model_substitution", .bool(automatic)))))
                        effects.append(.send(.delegate(.set("substitution_was_edited", .bool(false)))))
                    }
                }
                return .concatenate(effects)
            case .toggle(let id, let enabled):
                guard state.field(id) != nil else { return .none }
                state.values[id] = .bool(enabled)
                if id == "allow_model_substitution" {
                    state.values["substitution_was_edited"] = .bool(true)
                    return .concatenate(.send(.delegate(.set(id, .bool(enabled)))), .send(.delegate(.set("substitution_was_edited", .bool(true)))))
                }
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


public struct RequestChoiceGroup: Equatable, Identifiable, Sendable {
    public let title: String
    public var items: [ChoiceItem]
    public var id: String { title }
}
