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
        public let project: String?
        public let touchedAt: Date?
        public let link: AlondraContract.Link?
        public let withdraw: WireAction?
        public let tasks: [Line]

        public var isSingleTask: Bool { tasks.count == 1 }
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
    }

    public struct Fold: Equatable, Sendable {
        public let id: String
        public let title: String
        public let count: Int
        public let requests: [Request]
        public let more: More?
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
            let requests = (rows(of: section) + (page?.rows ?? [])).map(Request.init)
            return Fold(
                id: id, title: section?.title ?? fallbackTitle, count: Int(section?.count ?? 0),
                requests: requests, more: page.map(\.more) ?? section?.more
            )
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

/// Rows loaded by "show more" for one section, and the page after them if there is one.
public struct TasksPage: Equatable, Sendable {
    public var rows: [Row]
    public var more: More?

    public init(rows: [Row], more: More?) {
        self.rows = rows
        self.more = more
    }
}

extension TasksBoard.Request {
    init(row: Row) {
        self.init(
            id: row.id, title: row.title, state: row.subtitle,
            project: row.badges.first?.text, touchedAt: row.trailing?.instant, link: row.link,
            withdraw: row.actions.first { $0.command == "withdraw" },
            tasks: TasksBoard.Line.tree(row.children, rails: [])
        )
    }
}

extension TasksBoard.Line {
    static func tree(_ rows: [Row], rails: [Bool]) -> [Self] {
        rows.enumerated().flatMap { index, row -> [Self] in
            let last = index == rows.count - 1
            let activity = row.children.first { $0.id == "activity" }
            let children = row.children.filter { $0.id != "activity" }
            var activityAt: String?
            if case .time(let at, _)? = activity?.trailing { activityAt = at }
            let line = Self(
                id: row.id, title: row.title, state: row.subtitle, tone: row.badges.first?.tone,
                icon: row.icon, struck: row.struck, waiting: row.trailing?.count ?? 0,
                activity: activity?.title, activityAt: activityAt,
                depth: rails.count, rails: rails, isLastSibling: last, hasChildren: !children.isEmpty,
                link: row.link
            )
            return [line] + tree(children, rails: rails + [!last])
        }
    }
}
