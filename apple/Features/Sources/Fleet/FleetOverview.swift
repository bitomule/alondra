import AlondraContract
import Foundation

/// What the Fleet list reads from the `fleet` view, and from the `search?q=` view that has the same
/// shape: the orchestrator row, the pace of the fleet, then one group of nodes per project.
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
        public var isOrchestrator: Bool
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
            isOrchestrator = row.icon == .orchestrator
            let activity = row.children.first { $0.id == "activity" }
            self.activity = activity?.title
            if case .time(let at, _)? = activity?.trailing { activityAt = at }
            interrupt = row.actions.first { $0.id?.hasPrefix("interrupt:") == true }
            close = row.actions.first { $0.id?.hasPrefix("close:") == true }
            pause = row.actions.first { $0.command == "pause" }
            resume = row.actions.first { $0.command == "resume" }
        }
    }

    /// How many nodes work against the limit, where the limit comes from, and every task that
    /// waits its turn or is paused, in the order it comes back. The limit changes in Settings.
    public struct Pacing: Equatable, Sendable {
        public struct Waiting: Equatable, Sendable, Identifiable {
            public var id: String
            public var title: String
            public var state: String?
            public var link: AlondraContract.Link?
            public var paused: Bool
            /// Pause for a queued task, Resume for a paused one.
            public var action: WireAction?
        }

        public var summary: String
        public var limitText: String?
        public var waiting: [Waiting] = []

        init?(section: ViewSection) {
            guard section.id == "pacing" else { return nil }
            var summary: String?
            for item in section.items {
                switch item {
                case .row(let row) where row.id == "pacing":
                    summary = row.title
                    limitText = row.subtitle
                case .row(let row):
                    let paused = row.id.hasPrefix("paused:")
                    waiting.append(Waiting(id: row.id, title: row.title, state: row.subtitle, link: row.link, paused: paused,
                                           action: row.actions.first { $0.command == (paused ? "resume" : "pause") }))
                default:
                    break
                }
            }
            guard let summary else { return nil }
            self.summary = summary
        }
    }

    public struct Group: Equatable, Sendable, Identifiable {
        public var id: String
        public var title: String?
        public var count: UInt32?
        public var collapsed = false
        public var nodes: [Node]
    }

    public var orchestrator: Node?
    public var pacing: Pacing?
    public var groups: [Group] = []
    public var empty: Notice?

    public init(document: ViewDocument) {
        empty = document.empty
        for section in document.sections {
            if let pacing = Pacing(section: section) {
                self.pacing = pacing
                continue
            }
            let nodes = section.items.compactMap { item -> Node? in
                if case .row(let row) = item { return Node(row: row) }
                return nil
            }
            if section.id == "orchestrator" {
                orchestrator = nodes.first
            } else if !nodes.isEmpty {
                groups.append(Group(id: section.id, title: section.title, count: section.count, collapsed: section.collapsed, nodes: nodes))
            }
        }
    }

    public var isEmpty: Bool { orchestrator == nil && groups.isEmpty }

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
        overview.orchestrator = orchestrator.map(shown)
        if var pacing {
            pacing.waiting = pacing.waiting.map { waiting in
                guard let action = waiting.action, optimistic.hides(action.id) else { return waiting }
                var waiting = waiting
                waiting.state = action.stamp ?? waiting.state
                waiting.paused = action.command == "pause"
                waiting.action = nil
                return waiting
            }
            overview.pacing = pacing
        }
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
