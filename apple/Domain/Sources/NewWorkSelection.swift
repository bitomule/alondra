import AlondraContracts

/// The choices for one new request. The server's catalog remains authoritative for defaults.
public struct NewWorkSelection: Sendable, Equatable {
    public var project: String? {
        didSet {
            guard project != oldValue else { return }
            specs = []
            chosen = [:]
            issue = nil
            error = nil
        }
    }
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
        guard specs.contains(spec) else { return }
        chosen[spec.id] = choice == spec.default ? nil : choice
    }

    /// An issue may select a different checkout. Change project first so its reset keeps the new issue.
    public mutating func select(issue: String?, inferredProject: String?) {
        if let inferredProject, inferredProject != project {
            project = inferredProject
        }
        self.issue = issue
    }

    /// Ignore late responses for a different project and discard no-longer-valid overrides.
    public mutating func apply(_ catalog: OptionsCatalog) {
        guard catalog.project == project else { return }
        specs = catalog.options
        error = catalog.error
        chosen = chosen.filter { id, choice in
            specs.first(where: { $0.id == id }).map { accepts(choice, for: $0) && $0.default != choice } ?? false
        }
    }

    public func body(text: String, attachments: [String] = []) -> NewRequestBody {
        NewRequestBody(text: text, attachments: attachments, project: project, options: chosen, issue: issue)
    }

    private func accepts(_ choice: OptionChoice, for spec: OptionSpec) -> Bool {
        switch (spec.kind, choice) {
        case (.toggle, .toggle): true
        case (.choice, .value(let id)): spec.values.contains { $0.id == id }
        default: false
        }
    }
}
