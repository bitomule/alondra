import AlondraContract
import Foundation

/// What the Fleet list reads from the `fleet` view, and from the `search?q=` view that has the same
/// shape: one group of nodes per project.
public struct FleetOverview: Equatable, Sendable {
    /// A node and its one task as one item: the task's title, the node's name when it adds
    /// something, its state, and what it does now or said last.
    public struct Node: Equatable, Sendable, Identifiable {
        public var id: String
        public var title: String
        public var subtitle: String?
        /// The node's one state, first among the row's badges.
        public var state: AlondraContract.Label?
        public var badges: [AlondraContract.Label]
        public var pending: UInt32
        public var link: AlondraContract.Link?
        public var activity: String?
        public var activityAt: String?
        public var interrupt: WireAction?
        public var close: WireAction?
        public var pause: WireAction?
        public var resume: WireAction?
        /// What a Pause or Resume just sent says until the daemon's view shows it.
        public var stamp: String?

        init(row: Row) {
            id = row.id
            title = row.title
            subtitle = row.subtitle
            state = row.badges.first
            badges = Array(row.badges.dropFirst())
            if case .count(let value)? = row.trailing { pending = value } else { pending = 0 }
            link = row.link
            let activity = row.children.first { $0.id == "activity" }
            self.activity = activity?.title
            if case .time(let at, _)? = activity?.trailing { activityAt = at }
            interrupt = row.actions.first { $0.id?.hasPrefix("interrupt:") == true }
            close = row.actions.first { $0.id?.hasPrefix("close:") == true }
            pause = row.actions.first { $0.command == "pause" }
            resume = row.actions.first { $0.command == "resume" }
        }
    }

    public struct Group: Equatable, Sendable, Identifiable {
        public var id: String
        public var title: String?
        public var count: UInt32?
        public var collapsed = false
        public var nodes: [Node]
    }

    public var groups: [Group] = []
    public var empty: Notice?

    public init(document: ViewDocument) {
        empty = document.empty
        for section in document.sections {
            let nodes = section.items.compactMap { item -> Node? in
                if case .row(let row) = item { return Node(row: row) }
                return nil
            }
            if section.id != Self.internalSection, !nodes.isEmpty {
                groups.append(Group(id: section.id, title: section.title, count: section.count, collapsed: section.collapsed, nodes: nodes))
            }
        }
    }

    /// What a daemon from before the orchestrator became internal still sends; it is not a node.
    static let internalSection = "orchestrator"

    public var isEmpty: Bool { groups.isEmpty }

    /// The list as it is after what was just sent: a closing node is gone, an interrupted one no
    /// longer offers Interrupt, and a paused or resumed one says so.
    public func hiding(_ optimistic: Optimistic) -> FleetOverview {
        guard !optimistic.isEmpty else { return self }
        func shown(_ node: Node) -> Node {
            var node = node
            if optimistic.hides(node.interrupt?.id) { node.interrupt = nil }
            for action in [node.pause, node.resume].compactMap({ $0 }) where optimistic.hides(action.id) {
                (node.pause, node.resume, node.interrupt) = (nil, nil, nil)
                node.stamp = action.stamp
            }
            return node
        }
        var overview = self
        overview.groups = groups.compactMap { group in
            var group = group
            let before = group.nodes.count
            group.nodes = group.nodes.filter { !optimistic.hides($0.close?.id) }.map(shown)
            let closing = UInt32(before - group.nodes.count)
            group.count = group.count.map { $0 - min($0, closing) }
            return group.nodes.isEmpty ? nil : group
        }
        return overview
    }
}
