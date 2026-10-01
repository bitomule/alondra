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

    public struct Section<Element: Equatable & Sendable>: Equatable, Sendable {
        public let title: String?
        public let elements: [Element]
    }

    public let title: String
    public let state: String?
    public let stateTone: Tone?
    public let since: Date?
    public let node: Reference?
    public let request: Reference?
    public let now: Reference?
    public let waiting: Section<AlondraContract.Card>
    public let requestText: Section<String>
    public let pullRequests: Section<Row>
    public let dependencies: Section<Row>
    public let documents: Section<Row>
    public let options: Section<Fact>
    public let history: Section<Step>
    public let actions: [WireAction]

    public var cancel: WireAction? { action("cancel") }
    public var reopen: WireAction? { action("reopen") }
    public var archive: WireAction? { action("archive") ?? action("unarchive") }

    private func action(_ verb: String) -> WireAction? { actions.first { $0.command == verb } }
}

public extension TaskDetail {
    init(document: ViewDocument) {
        func section(_ id: String) -> ViewSection? { document.sections.first { $0.id == id } }
        func items(_ id: String) -> [ViewItem] { section(id)?.items ?? [] }
        func rows(_ id: String) -> Section<Row> {
            Section(title: section(id)?.title, elements: items(id).compactMap { if case .row(let row) = $0 { row } else { nil } })
        }
        let facts = items("header").flatMap { item -> [Fact] in
            if case .facts(let facts) = item { facts.items } else { [] }
        }
        func reference(_ fact: Fact?) -> Reference? { fact.map { Reference(text: $0.value, link: $0.link) } }
        func linked(to prefix: String) -> Fact? {
            facts.first { if case .view(let key)? = $0.link { key.hasPrefix(prefix) } else { false } }
        }
        let state = facts.first { $0.link == nil }
        let now = items("now").compactMap { item -> Reference? in
            if case .row(let row) = item { Reference(text: row.title, link: row.link) } else { nil }
        }.first
        self.init(
            title: document.title ?? "",
            state: state?.value,
            stateTone: state?.tone,
            since: items("since").compactMap { item -> Date? in
                if case .row(let row) = item { row.trailing?.instant } else { nil }
            }.first,
            node: reference(linked(to: "node:")),
            request: reference(linked(to: "request:")),
            now: now,
            waiting: Section(title: section("waiting")?.title, elements: items("waiting").compactMap {
                if case .card(let card) = $0 { card } else { nil }
            }),
            requestText: Section(title: section("request")?.title, elements: items("request").compactMap {
                if case .markdown(let markdown) = $0, !markdown.text.isEmpty { markdown.text } else { nil }
            }),
            pullRequests: rows("pull_requests"),
            dependencies: rows("dependencies"),
            documents: rows("documents"),
            options: Section(title: section("options")?.title, elements: items("options").flatMap { item -> [Fact] in
                if case .facts(let facts) = item { facts.items } else { [] }
            }),
            history: Section(title: section("history")?.title, elements: items("history").compactMap { item -> Step? in
                guard case .row(let row) = item else { return nil }
                return Step(id: row.id, state: row.title, summary: row.subtitle, at: row.trailing?.instant)
            }),
            actions: items("actions").flatMap { item -> [WireAction] in
                if case .actions(let bar) = item { bar.actions } else { [] }
            }
        )
    }
}
