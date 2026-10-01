import AlondraContract
import Foundation

/// What the Fleet list reads from the `fleet` view, and from the `search?q=` view that has the same
/// shape: the orchestrator row, then one group of nodes per project.
public struct FleetOverview: Equatable, Sendable {
    public struct Node: Equatable, Sendable, Identifiable {
        public var id: String
        public var title: String
        public var state: String?
        public var badges: [AlondraContract.Label]
        public var pending: UInt32
        public var link: AlondraContract.Link?
        public var isOrchestrator: Bool
        public var lastMessage: Row?
        public var tasks: [Row]
        public var close: WireAction?

        init(row: Row) {
            id = row.id
            title = row.title
            state = row.subtitle
            badges = row.badges
            if case .count(let value)? = row.trailing { pending = value } else { pending = 0 }
            link = row.link
            isOrchestrator = row.icon == .orchestrator
            lastMessage = row.children.first { $0.id == "last_message" }
            tasks = row.children.filter { $0.id != "last_message" }
            close = row.actions.first { $0.id?.hasPrefix("close:") == true }
        }
    }

    public struct Group: Equatable, Sendable, Identifiable {
        public var id: String
        public var title: String?
        public var count: UInt32?
        public var nodes: [Node]
    }

    public var orchestrator: Node?
    public var groups: [Group] = []
    public var empty: Notice?

    public init(document: ViewDocument) {
        empty = document.empty
        for section in document.sections {
            let nodes = section.items.compactMap { item -> Node? in
                if case .row(let row) = item { return Node(row: row) }
                return nil
            }
            if section.id == "orchestrator" {
                orchestrator = nodes.first
            } else if !nodes.isEmpty {
                groups.append(Group(id: section.id, title: section.title, count: section.count, nodes: nodes))
            }
        }
    }

    public var isEmpty: Bool { orchestrator == nil && groups.isEmpty }
}
