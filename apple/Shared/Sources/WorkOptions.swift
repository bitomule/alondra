import Foundation
import Observation

/// A value of an option for new work: a toggle's on/off, or the id of a choice's value.
public enum OptionChoice: Codable, Hashable, Sendable {
    case toggle(Bool)
    case value(String)

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if let b = try? c.decode(Bool.self) {
            self = .toggle(b)
        } else {
            self = .value(try c.decode(String.self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .toggle(let b): try c.encode(b)
        case .value(let v): try c.encode(v)
        }
    }
}

/// One option as alondrad serves it: built in (PR handling, effort) or declared by the user.
public struct OptionSpec: Decodable, Identifiable, Hashable, Sendable {
    public enum Kind: String, Decodable, Sendable {
        case choice, toggle
    }

    public struct Value: Decodable, Identifiable, Hashable, Sendable {
        public let id: String
        public let label: String
    }

    public let id: String
    public let label: String
    public let kind: Kind
    public let values: [Value]
    public let `default`: OptionChoice
    public let builtin: Bool

    enum CodingKeys: String, CodingKey {
        case id, label, kind, values, `default`, builtin
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        label = try c.decode(String.self, forKey: .label)
        kind = try c.decode(Kind.self, forKey: .kind)
        values = try c.decodeIfPresent([Value].self, forKey: .values) ?? []
        self.default = try c.decode(OptionChoice.self, forKey: .default)
        builtin = try c.decodeIfPresent(Bool.self, forKey: .builtin) ?? false
    }

    /// The user's own words for their options; the built-ins in the app's language.
    public var title: String {
        guard builtin else { return label }
        switch id {
        case "pr": return String(localized: "PRs", table: "AlondraShared", bundle: #bundle)
        case "effort": return String(localized: "Effort", table: "AlondraShared", bundle: #bundle)
        default: return label
        }
    }

    public func title(of value: Value) -> String {
        guard builtin else { return value.label }
        switch value.id {
        case "review": return String(localized: "Review each one", table: "AlondraShared", bundle: #bundle)
        case "auto_merge": return String(localized: "Auto-merge", table: "AlondraShared", bundle: #bundle)
        case "light": return String(localized: "Light", table: "AlondraShared", bundle: #bundle)
        case "normal": return String(localized: "Normal", table: "AlondraShared", bundle: #bundle)
        case "hard": return String(localized: "Hard", table: "AlondraShared", bundle: #bundle)
        default: return value.label
        }
    }

    public func title(of choice: OptionChoice) -> String {
        switch choice {
        case .toggle(true): String(localized: "Yes", table: "AlondraShared", bundle: #bundle)
        case .toggle(false): String(localized: "No", table: "AlondraShared", bundle: #bundle)
        case .value(let id): values.first { $0.id == id }.map(title(of:)) ?? id
        }
    }

    func accepts(_ choice: OptionChoice) -> Bool {
        switch (kind, choice) {
        case (.toggle, .toggle): true
        case (.choice, .value(let id)): values.contains { $0.id == id }
        default: false
        }
    }
}

public struct OptionsCatalog: Decodable, Sendable {
    public let project: String?
    public let options: [OptionSpec]
    public let error: String?
}

public struct ProjectList: Decodable, Sendable {
    public struct Project: Decodable, Hashable, Sendable {
        public let name: String
    }

    public let projects: [Project]
}

public struct NewRequestBody: Encodable, Sendable, Equatable {
    public let text: String
    public var attachments: [String] = []
    public let project: String?
    public let options: [String: OptionChoice]
    public var issue: String?
}

/// What the user picked in "New". Only what differs from the defaults is kept, so alondrad
/// fills in the rest and a default changed in `options.toml` applies to the next request.
public struct WorkOptions: Sendable, Equatable {
    public var project: String?
    /// `owner/repo#n` the work comes from; alondrad passes its text on and the node's PR closes it.
    public var issue: String?
    public private(set) var specs: [OptionSpec] = []
    public private(set) var chosen: [String: OptionChoice] = [:]
    public private(set) var error: String?

    public init(project: String? = nil) {
        self.project = project
    }

    public func current(_ spec: OptionSpec) -> OptionChoice {
        chosen[spec.id] ?? spec.default
    }

    public mutating func set(_ spec: OptionSpec, to choice: OptionChoice) {
        chosen[spec.id] = choice == spec.default ? nil : choice
    }

    /// A fresh catalog, for this project: choices it no longer offers are dropped.
    public mutating func apply(_ catalog: OptionsCatalog) {
        guard catalog.project == project else { return }
        specs = catalog.options
        error = catalog.error
        chosen = chosen.filter { id, choice in
            specs.first { $0.id == id }.map { $0.accepts(choice) && $0.default != choice } ?? false
        }
    }

    public func body(text: String) -> NewRequestBody {
        NewRequestBody(text: text, project: project, options: chosen, issue: issue)
    }

    public var summary: String {
        specs.map { "\($0.title): \($0.title(of: current($0)))" }.joined(separator: " · ")
    }
}

/// Loads the options and projects for "New" when it opens or its project changes; nothing
/// polls them.
@MainActor
@Observable
public final class WorkOptionsModel {
    public var options = WorkOptions()
    public private(set) var projects: [String] = []
    public private(set) var loaded = false
    private let client: AlondraClient

    public init(client: AlondraClient) {
        self.client = client
    }

    public func load() async {
        if projects.isEmpty, let list = try? await client.projects() {
            projects = list.projects.map(\.name)
        }
        let project = options.project
        guard let catalog = try? await client.options(project: project) else { return }
        options.apply(catalog)
        loaded = true
    }

    public func choose(project: String?) async {
        options.project = project
        await load()
    }

    /// Picks the issue, and its repo's project when the user has a checkout of that name.
    public func choose(issue: String?) async {
        options.issue = issue
        guard let project = IssueSummary.project(of: issue, among: projects), project != options.project else { return }
        await choose(project: project)
    }
}
