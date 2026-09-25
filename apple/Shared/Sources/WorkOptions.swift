import AlondraContracts
import AlondraDomain
import Foundation
import Observation

public typealias OptionChoice = AlondraContracts.OptionChoice
public typealias OptionSpec = AlondraContracts.OptionSpec
public typealias OptionsCatalog = AlondraContracts.OptionsCatalog
public typealias ProjectList = AlondraContracts.ProjectList
public typealias NewRequestBody = AlondraContracts.NewRequestBody
public typealias WorkOptions = AlondraDomain.NewWorkSelection

extension OptionSpec {
    /// The user's own words for their options; the built-ins in the app's language.
    public var title: String {
        guard builtin else { return label }
        switch id {
        case "pr": return String(localized: "PRs", table: "AlondraShared", bundle: #bundle)
        case "effort": return String(localized: "Effort", table: "AlondraShared", bundle: #bundle)
        default: return label
        }
    }

    /// How a task's PRs are handled, in the words of the launch option.
    public static func prHandling(autoMerge: Bool) -> String {
        autoMerge
            ? String(localized: "Auto-merge", table: "AlondraShared", bundle: #bundle)
            : String(localized: "Review each one", table: "AlondraShared", bundle: #bundle)
    }

    public func title(of value: Value) -> String {
        guard builtin else { return value.label }
        switch value.id {
        case "review": return Self.prHandling(autoMerge: false)
        case "auto_merge": return Self.prHandling(autoMerge: true)
        case "auto": return String(localized: "Auto", table: "AlondraShared", bundle: #bundle)
        case "light": return String(localized: "Light", table: "AlondraShared", bundle: #bundle)
        case "normal": return String(localized: "Normal", table: "AlondraShared", bundle: #bundle)
        case "hard": return String(localized: "Hard", table: "AlondraShared", bundle: #bundle)
        default: return value.label
        }
    }

    /// The line under a value: the built-ins' in the app's language, the user's from `options.toml`.
    public func description(of value: Value) -> String? {
        guard builtin else { return value.description }
        switch value.id {
        case "review": return String(localized: "Each PR is a card for you; the node waits for your merge", table: "AlondraShared", bundle: #bundle)
        case "auto_merge": return String(localized: "Alondra merges the node’s PR once its checks pass", table: "AlondraShared", bundle: #bundle)
        case "auto": return String(localized: "The orchestrator picks the model and effort from the task", table: "AlondraShared", bundle: #bundle)
        case "light": return String(localized: "A quick, cheaper model for small, clear work", table: "AlondraShared", bundle: #bundle)
        case "normal": return String(localized: "Claude’s default model and effort", table: "AlondraShared", bundle: #bundle)
        case "hard": return String(localized: "The strongest model and the most effort", table: "AlondraShared", bundle: #bundle)
        default: return value.description
        }
    }

    /// The values a picker lists: the option's own, or yes and no for a toggle.
    public var choices: [OptionChoice] {
        switch kind {
        case .toggle: [.toggle(true), .toggle(false)]
        case .choice: values.map { .value($0.id) }
        }
    }

    public func description(of choice: OptionChoice) -> String? {
        guard case .value(let id) = choice, let value = values.first(where: { $0.id == id }) else { return nil }
        return description(of: value)
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

extension ProjectList {
    /// Most recently used first, as alondrad ranks them.
    public static func filter(_ ranked: [String], query: String) -> [String] {
        let words = query.lowercased().split(whereSeparator: \.isWhitespace)
        return ranked.filter { name in words.allSatisfy { name.lowercased().contains($0) } }
    }
}

extension WorkOptions {
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
    private var loadGeneration: UInt64 = 0

    public init(client: AlondraClient) {
        self.client = client
    }

    public func load() async {
        loadGeneration &+= 1
        let generation = loadGeneration
        if projects.isEmpty, let list = try? await client.projects() {
            guard generation == loadGeneration else { return }
            projects = list.projects.map(\.name)
        }
        let project = options.project
        guard let catalog = try? await client.options(project: project) else { return }
        guard generation == loadGeneration, options.project == project else { return }
        options.apply(catalog)
        loaded = true
    }

    public func choose(project: String?) async {
        options.project = project
        loaded = false
        await load()
    }

    /// Picks the issue, and its repo's project when the user has a checkout of that name.
    public func choose(issue: String?) async {
        let previousProject = options.project
        options.select(issue: issue, inferredProject: IssueSummary.project(of: issue, among: projects))
        if options.project != previousProject {
            loaded = false
            await load()
        }
    }
}
