import AlondraContract
import Foundation

/// The `task:{id}` view as the task screen lays it out. Each part is read from the section the
/// daemon names it by; a section this screen does not know is ignored.
public struct TaskDetail: Equatable, Sendable {
    public struct Reference: Equatable, Sendable {
        public let text: String
        public let link: AlondraContract.Link?
    }

    public struct Step: Equatable, Sendable, Identifiable {
        public let id: String
        public let state: String
        public let summary: String?
        public let at: Date?
    }

    /// Something the task's node is doing or left running: a thread in its turn, a shell, a
    /// subagent.
    public struct Activity: Equatable, Sendable, Identifiable {
        public let id: String
        public let title: String
        public let detail: String?
        public let since: String?
    }

    /// One part of how far the node's work has got: a line of facts or a step, subagent or agent.
    public enum Progress: Equatable, Sendable {
        case facts([Fact])
        case row(Row)
    }

    public struct Section<Element: Equatable & Sendable>: Equatable, Sendable {
        public let title: String?
        public let elements: [Element]
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
    public let workflows: Section<Progress>
    public let background: Section<Activity>
    public let lastMessage: Message?
    public let waiting: Section<AlondraContract.Card>
    public let requestText: Section<String>
    public let pullRequests: Section<Row>
    public let dependencies: Section<Row>
    public let documents: Section<Row>
    public let options: Section<Fact>
    public let history: Section<Step>
    public let actions: [WireAction]

    public var relaunch: WireAction? { action("relaunch") }
    public var cancel: WireAction? { action("cancel") }
    public var reopen: WireAction? { action("reopen") }
    public var archive: WireAction? { action("archive") ?? action("unarchive") }
    public var pause: WireAction? { action("pause") }
    public var resume: WireAction? { action("resume") }
    public var priority: WireAction? { action("set_priority") }
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
            Section(title: section(id)?.title, elements: rowsOf(id).map(Activity.init(row:)))
        }
        func progress(_ id: String) -> Section<Progress> {
            Section(title: section(id)?.title, elements: items(id).compactMap { item -> Progress? in
                switch item {
                case .facts(let facts): .facts(facts.items)
                case .row(let row): .row(row)
                default: nil
                }
            })
        }
        let facts = factsOf("header")
        let state = facts.first { $0.link == nil }
        let now = rowsOf("now").first.map { Reference(text: $0.title, link: $0.link) }
        self.init(
            title: document.title ?? "",
            state: state?.value,
            stateTone: state?.tone,
            since: rowsOf("since").compactMap { $0.trailing?.instant }.first,
            folder: facts.dropFirst(2).first { $0.link == nil && $0.value.hasPrefix("/") }?.value,
            request: facts.dropFirst().first { $0.link == nil }.map { Reference(text: $0.value, link: $0.link) },
            now: now,
            readings: factsOf("readings"),
            activity: activities("activity"),
            needs: Section(title: section("needs")?.title, elements: markdown("needs")),
            steps: progress("steps"),
            subagents: progress("subagents"),
            workflows: progress("workflows"),
            background: activities("background"),
            lastMessage: markdown("last_message").first.map { markdown in
                Message(title: section("last_message")?.title, markdown: markdown,
                        at: rowsOf("last_message").compactMap(\.time).first)
            },
            waiting: Section(title: section("waiting")?.title, elements: items("waiting").compactMap {
                if case .card(let card) = $0 { card } else { nil }
            }),
            requestText: Section(title: section("request")?.title, elements: markdown("request").map(\.text)),
            pullRequests: rows("pull_requests"),
            dependencies: rows("dependencies"),
            documents: rows("documents"),
            options: Section(title: section("options")?.title, elements: factsOf("options")),
            history: Section(title: section("history")?.title, elements: rowsOf("history").map { row in
                Step(id: row.id, state: row.title, summary: row.subtitle, at: row.trailing?.instant)
            }),
            actions: items("actions").flatMap { item -> [WireAction] in
                if case .actions(let bar) = item { bar.actions } else { [] }
            }.filter(\.enabled)
        )
    }
}

private extension TaskDetail.Activity {
    init(row: Row) {
        self.init(id: row.id, title: row.title, detail: row.subtitle, since: row.time)
    }
}

private extension Row {
    var time: String? {
        if case .time(let at, _)? = trailing { at } else { nil }
    }
}
