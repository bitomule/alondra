import AlondraContract
import Foundation

/// The `task:{id}` view as the task screen lays it out. Each part is read from the section the
/// daemon names it by; a section this screen does not know is ignored.
public struct TaskDetail: Equatable, Sendable {
    public struct Reference: Equatable, Sendable {
        public let text: String
        public let link: AlondraContract.Link?
    }

    /// Something the task's node is doing or left running: a thread in its turn, a shell, a
    /// subagent.
    public struct Activity: Equatable, Sendable, Identifiable {
        public let id: String
        public let title: String
        public let detail: String?
        public let since: String?
        /// Whether it runs or how it ended, as the Mac says it: «En marcha», «Terminado», «Falló», «Detenido».
        public let state: Label?
    }

    public struct RunTime: Equatable, Sendable {
        public let title: String
        public let label: AlondraContract.Label
    }

    /// One part of how far the node's work has got: a line of facts or a step, subagent or agent.
    public enum Progress: Equatable, Sendable {
        case facts([Fact])
        case row(Row)
    }

    /// A command the task lets through without asking: what it allows, from where and how often
    /// it was used, and its Revoke.
    public struct Grant: Equatable, Sendable, Identifiable {
        public let id: String
        public let command: String
        public let detail: String?
        public let revoke: WireAction?
    }

    /// A part of the screen. One the daemon sends `collapsed` starts closed and opens on a tap; a
    /// daemon older than that sends every section open.
    public struct Section<Element: Equatable & Sendable>: Equatable, Sendable {
        public let title: String?
        public let elements: [Element]
        public var collapsed = false
        public var count: Int?
    }

    public struct Message: Equatable, Sendable {
        public let title: String?
        public let markdown: Markdown
        public let at: String?
    }

    public let title: String
    public let state: String?
    public let stateTone: Tone?
    public let since: Date?
    /// "Running for" or "Took" and the label whose time the app counts.
    public let runTime: RunTime?
    public let folder: String?
    public let request: Reference?
    public let now: Reference?
    /// What the node reports while it works: the tool it runs, what it left in the background, its
    /// last turn, context and cost.
    public let readings: [Fact]
    public let activity: Section<Activity>
    public let needs: Section<Markdown>
    public let steps: Section<Progress>
    public let subagents: Section<Progress>
    /// Subagents that ended, once two or more have: folded, so the screen shows what runs.
    public let subagentsFinished: Section<Progress>
    public let workflows: Section<Progress>
    public let background: Section<Activity>
    public let backgroundFinished: Section<Activity>
    public let lastMessage: Message?
    public let waiting: Section<AlondraContract.Card>
    public let requestText: Section<String>
    public let pullRequests: Section<Row>
    public let dependencies: Section<Row>
    public let documents: Section<Row>
    public let options: Section<Fact>
    public let allowed: Section<Grant>
    public let actions: [WireAction]

    public var relaunch: WireAction? { action("relaunch") }
    public var cancel: WireAction? { action("cancel") }
    public var reopen: WireAction? { action("reopen") }
    public var pause: WireAction? { action("pause") }
    public var resume: WireAction? { action("resume") }
    public var priority: WireAction? { action("set_priority") }
    /// The priority menu drawn in place of `fact`: only the fact the daemon marks as the priority,
    /// so an older daemon that marks none shows the priority as text.
    public func priorityMenu(for fact: Fact) -> WireAction? { fact.kind == .priority ? priority : nil }
    public var modelSelection: WireAction? {
        actions.first { if case .view(let key) = $0.link { key.hasPrefix("task_selection:") } else { false } }
    }
    public var message: WireAction? { action("msg") }
    /// Interrupt, or Force stop once Claude accepted the interrupt and its turn still runs.
    public var interrupt: WireAction? { action("interrupt") ?? action("force_stop") }

    private func action(_ verb: String) -> WireAction? { actions.first { $0.command == verb } }
}

public extension TaskDetail {
    init(document: ViewDocument) {
        func section(_ id: String) -> ViewSection? { document.sections.first { $0.id == id } }
        func items(_ id: String) -> [ViewItem] { section(id)?.items ?? [] }
        func rowsOf(_ id: String) -> [Row] { items(id).compactMap { if case .row(let row) = $0 { row } else { nil } } }
        func rows(_ id: String) -> Section<Row> { Section(title: section(id)?.title, elements: rowsOf(id)) }
        func factsOf(_ id: String) -> [Fact] {
            items(id).flatMap { item -> [Fact] in if case .facts(let facts) = item { facts.items } else { [] } }
        }
        func markdown(_ id: String) -> [Markdown] {
            items(id).compactMap { if case .markdown(let markdown) = $0, !markdown.text.isEmpty { markdown } else { nil } }
        }
        func activities(_ id: String) -> Section<Activity> {
            Section(title: section(id)?.title, elements: rowsOf(id).map(Activity.init(row:)),
                    collapsed: section(id)?.collapsed ?? false, count: section(id)?.count.map(Int.init))
        }
        func progress(_ id: String) -> Section<Progress> {
            Section(title: section(id)?.title, elements: items(id).compactMap { item -> Progress? in
                switch item {
                case .facts(let facts): .facts(facts.items)
                case .row(let row): .row(row)
                default: nil
                }
            }, collapsed: section(id)?.collapsed ?? false, count: section(id)?.count.map(Int.init))
        }
        let facts = factsOf("header")
        let state = facts.first { $0.kind == .state }
        let now = rowsOf("now").first.map { Reference(text: $0.title, link: $0.link) }
        self.init(
            title: document.title ?? "",
            state: state?.value,
            stateTone: state?.tone,
            since: rowsOf("since").compactMap { $0.trailing?.instant }.first,
            runTime: rowsOf("elapsed").first.flatMap { row in
                row.badges.first { $0.elapsed != nil }.map { RunTime(title: row.title, label: $0) }
            },
            folder: facts.first { $0.kind == .folder }?.value,
            request: facts.first { $0.kind == .request }.map { Reference(text: $0.value, link: $0.link) },
            now: now,
            readings: factsOf("readings"),
            activity: activities("activity"),
            needs: Section(title: section("needs")?.title, elements: markdown("needs")),
            steps: progress("steps"),
            subagents: progress("subagents"),
            subagentsFinished: progress("subagents_finished"),
            workflows: progress("workflows"),
            background: activities("background"),
            backgroundFinished: activities("background_finished"),
            lastMessage: markdown("last_message").first.map { markdown in
                Message(title: section("last_message")?.title, markdown: markdown,
                        at: rowsOf("last_message").compactMap(\.time).first)
            },
            waiting: Section(title: section("waiting")?.title, elements: items("waiting").compactMap {
                if case .card(let card) = $0 { card } else { nil }
            }),
            requestText: Section(title: section("request")?.title, elements: markdown("request").map(\.text),
                                 collapsed: section("request")?.collapsed ?? false),
            pullRequests: rows("pull_requests"),
            dependencies: rows("dependencies"),
            documents: rows("documents"),
            options: Section(title: section("options")?.title, elements: factsOf("options")),
            allowed: Section(title: section("allowed_in_task")?.title, elements: rowsOf("allowed_in_task").map { row in
                Grant(id: row.id, command: row.title, detail: row.subtitle, revoke: row.actions.first { $0.command == "revoke_grant" && $0.enabled })
            }),
            actions: items("actions").flatMap { item -> [WireAction] in
                if case .actions(let bar) = item { bar.actions } else { [] }
            }.filter(\.enabled)
        )
    }
}

private extension TaskDetail.Activity {
    init(row: Row) {
        self.init(id: row.id, title: row.title, detail: row.subtitle, since: row.time, state: row.badges.first)
    }
}

private extension Row {
    var time: String? {
        if case .time(let at, _)? = trailing { at } else { nil }
    }
}
