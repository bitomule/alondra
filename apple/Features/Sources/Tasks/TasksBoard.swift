import AlondraContract
import Foundation

/// The `tasks` view as the Tasks screen lays it out: requests with their task trees, the folds for
/// finished and archived work, and the entry to the scheduled tasks. The daemon supplies the
/// content; which section goes where is decided here, by section id.
public struct TasksBoard: Equatable, Sendable {
    public struct Request: Equatable, Sendable, Identifiable {
        public let id: String
        public let title: String
        public let state: String?
        public let icon: Icon
        public let project: String?
        public let touchedAt: Date?
        public let link: AlondraContract.Link?
        public let withdraw: WireAction?
        public let tasks: [Line]
        /// A task of a request that has no title and several tasks: it stands on its own, with no header.
        public let isLoose: Bool

        /// The request's own tasks: not the short rows for tasks that wait elsewhere, nor other requests' tasks nested here.
        public var taskCount: Int { tasks.count { !$0.isWait && !$0.isChain && $0.origin == nil } }
        public var isSingleTask: Bool { tasks.count == 1 && !tasks[0].isWait }

        /// The lines shown with the chains in `open` unfolded.
        public func lines(open: Set<String>) -> [Line] {
            tasks.filter { line in line.chain.map(open.contains) ?? true }
        }
    }

    /// One task in its request's tree, with what its node does now or said last. `rails` says, for
    /// each ancestor level, whether that ancestor has more tasks below this one.
    public struct Line: Equatable, Sendable, Identifiable {
        public let id: String
        public let title: String
        public let state: String?
        public let tone: Tone?
        public let icon: Icon
        public let struck: Bool
        public let waiting: Int
        public let activity: String?
        public let activityAt: String?
        public let depth: Int
        public let rails: [Bool]
        public let isLastSibling: Bool
        public let hasChildren: Bool
        public let link: AlondraContract.Link?
        /// "from: ‹request›" when this task waits under a task of another request.
        public let origin: String?
        /// A short row standing for a task that waits on another request's task: the task it waits for is
        /// the title, and `dependent` names the waiting task when its request has other tasks.
        public let isWait: Bool
        public let dependent: String?
        /// What the daemon says about the task beside its state, such as "Scheduled" for a scheduled run.
        public let marks: [String]
        /// "+7 waiting for this": the tasks that wait for the one above, folded until tapped.
        public let isChain: Bool
        /// The chain this line is folded into, hidden until that chain opens.
        public let chain: String?
    }

    public struct Fold: Equatable, Sendable {
        public let id: String
        public let title: String
        public let count: Int
        public let requests: [Request]
        public let more: More?
        /// The active work as the daemon orders it: For you, In progress, Waiting.
        public var blocks: [Block] = []
    }

    /// One block of the active work, under its title and count. Its entries are tasks on their own and requests
    /// with a header and several tasks.
    public struct Block: Equatable, Sendable, Identifiable {
        public let id: String
        public let title: String
        public let count: Int
        public let requests: [Request]
        /// Waiting work asks nothing of anyone: each task is two short lines, not a card.
        public var isCompact: Bool { id == "block:waiting" }
    }

    public struct Schedules: Equatable, Sendable {
        public let title: String
        public let count: Int
        public let link: AlondraContract.Link?
    }

    public let summary: String
    public let schedules: Schedules?
    public let active: Fold
    public let finished: Fold
    public let archived: Fold
    public let archiveCompleted: WireAction?
    public let empty: Notice?

    public var isEmpty: Bool { active.requests.isEmpty && finished.count == 0 && archived.count == 0 }
}

public extension TasksBoard {
    /// `pages` carries the rows "show more" loaded after the first twenty of a section.
    init(document: ViewDocument, pages: [String: TasksPage] = [:]) {
        func section(_ id: String) -> ViewSection? { document.sections.first { $0.id == id } }
        func rows(of section: ViewSection?) -> [Row] {
            (section?.items ?? []).compactMap { if case .row(let row) = $0 { row } else { nil } }
        }
        func fold(_ id: String, fallbackTitle: String) -> Fold {
            let section = section(id)
            let page = pages[id]
            let all = rows(of: section) + (page?.rows ?? [])
            let blocks = all.filter { $0.id.hasPrefix("block:") }.map { Block(row: $0) }
            let requests = blocks.isEmpty ? all.map { Request(row: $0) } : blocks.flatMap(\.requests)
            var fold = Fold(
                id: id, title: section?.title ?? fallbackTitle, count: Int(section?.count ?? 0),
                requests: requests, more: page.map(\.more) ?? section?.more
            )
            fold.blocks = blocks
            return fold
        }
        let entry = rows(of: section("scheduled")).first
        let actions = document.sections.lazy.flatMap(\.items).compactMap { item -> [WireAction]? in
            if case .actions(let bar) = item { bar.actions } else { nil }
        }.flatMap { $0 }
        self.init(
            summary: document.status.summary,
            schedules: entry.map {
                Schedules(title: $0.title, count: Int(section("scheduled")?.count ?? 0), link: $0.link)
            },
            active: fold("active", fallbackTitle: ""),
            finished: fold("finished", fallbackTitle: ""),
            archived: fold("archived", fallbackTitle: ""),
            archiveCompleted: actions.first { $0.command == "archive_done" },
            empty: document.empty
        )
    }
}

public extension TasksBoard {
    /// The board as it is after what was just sent: a withdrawn request is gone, archived work has
    /// moved to the archive.
    func hiding(_ optimistic: Optimistic) -> TasksBoard {
        guard !optimistic.isEmpty else { return self }
        func shown(_ fold: Fold, adding moved: Fold? = nil) -> Fold {
            let kept = fold.requests.filter { !optimistic.hides($0.withdraw?.id) }
            let movedRequests = moved?.requests ?? []
            var shown = Fold(id: fold.id, title: fold.title,
                             count: max(0, fold.count - (fold.requests.count - kept.count)) + (moved?.count ?? 0),
                             requests: movedRequests + kept, more: fold.more)
            shown.blocks = fold.blocks
            return shown
        }
        let archiving = optimistic.hides(archiveCompleted?.id)
        let emptied = Fold(id: finished.id, title: finished.title, count: 0, requests: [], more: nil)
        return TasksBoard(
            summary: summary, schedules: schedules, active: shown(active),
            finished: archiving ? emptied : shown(finished),
            archived: archiving ? shown(archived, adding: finished) : shown(archived),
            archiveCompleted: archiving ? nil : archiveCompleted, empty: empty
        )
    }
}

/// Rows loaded by "show more" for one section, and the page after them if there is one.
public struct TasksPage: Equatable, Sendable {
    public var rows: [Row]
    public var more: More?

    public init(rows: [Row], more: More?) {
        self.rows = rows
        self.more = more
    }
}

extension TasksBoard.Block {
    init(row: Row) {
        let requests = row.children.map { child in
            if child.id.hasPrefix("group:") { return TasksBoard.Request(group: child) }
            guard case .view(let key)? = child.link, key.hasPrefix("task:") else {
                var at: String?
                if case .time(let time, _)? = child.trailing { at = time }
                let line = TasksBoard.Line(
                    id: child.id, title: child.title, state: child.subtitle, tone: nil, icon: child.icon, struck: false,
                    waiting: 0, activity: nil, activityAt: at, depth: 0, rails: [], isLastSibling: true, hasChildren: false,
                    link: child.link, origin: nil, isWait: false, dependent: nil, marks: [], isChain: false, chain: nil
                )
                return TasksBoard.Request(
                    id: child.id, title: "", state: child.subtitle, icon: child.icon, project: nil,
                    touchedAt: child.trailing?.instant, link: child.link, withdraw: child.actions.first { $0.command == "withdraw" },
                    tasks: [line], isLoose: true
                )
            }
            return TasksBoard.Request(
                id: child.id, title: "", state: nil, icon: child.icon, project: nil, touchedAt: nil,
                link: child.link, withdraw: nil, tasks: TasksBoard.Line.tree([child], rails: [], chain: nil), isLoose: true
            )
        }
        let count: Int = if case .text(let text)? = row.trailing { Int(text) ?? requests.count } else { requests.count }
        self.init(id: row.id, title: row.title, count: count, requests: requests)
    }
}

extension TasksBoard.Request {
    /// A request with a real title and several tasks: its header, then its tasks.
    init(group row: Row) {
        self.init(
            id: row.id, title: row.title, state: nil, icon: row.icon, project: row.badges.first?.text,
            touchedAt: row.trailing?.instant, link: row.link, withdraw: row.actions.first { $0.command == "withdraw" },
            tasks: TasksBoard.Line.tree(row.children, rails: [], chain: nil), isLoose: false
        )
    }

    init(row: Row) {
        let loose = row.depth > 0
        self.init(
            id: row.id, title: loose ? "" : row.title, state: loose && row.id.hasPrefix("waits:") ? nil : row.subtitle,
            icon: row.icon, project: loose ? nil : row.badges.first?.text, touchedAt: loose ? nil : row.trailing?.instant,
            link: row.link, withdraw: row.actions.first { $0.command == "withdraw" },
            tasks: TasksBoard.Line.tree(loose ? [row] : row.children, rails: [], chain: nil), isLoose: loose
        )
    }
}

extension TasksBoard.Line {
    static func tree(_ rows: [Row], rails: [Bool], chain: String?) -> [Self] {
        rows.enumerated().flatMap { index, row -> [Self] in
            let last = index == rows.count - 1
            let activity = row.children.first { $0.id == "activity" }
            let origin = row.children.first { $0.id == "from" }
            let children = row.children.filter { $0.id != "activity" && $0.id != "from" }
            var activityAt: String?
            if case .time(let at, _)? = activity?.trailing { activityAt = at }
            let isWait = row.id.hasPrefix("waits:")
            let line = Self(
                id: row.id, title: row.title, state: isWait ? nil : row.subtitle, tone: row.badges.first?.tone,
                icon: row.icon, struck: row.struck, waiting: row.trailing?.count ?? 0,
                activity: activity?.title, activityAt: activityAt,
                depth: rails.count, rails: rails, isLastSibling: last, hasChildren: !children.isEmpty,
                link: row.link, origin: origin?.title, isWait: isWait, dependent: isWait ? row.subtitle : nil,
                marks: row.badges.dropFirst().map(\.text), isChain: row.folded, chain: chain
            )
            return [line] + tree(children, rails: rails + [!last], chain: row.folded ? row.id : chain)
        }
    }
}
