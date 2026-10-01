import AlondraContract
import Foundation

/// The `request:{id}` view: the request's state, its tasks and its text, with withdraw.
public struct RequestDetail: Equatable, Sendable {
    public struct Entry: Equatable, Sendable, Identifiable {
        public let id: String
        public let title: String
        public let state: String?
        public let tone: Tone?
        public let icon: Icon
        public let struck: Bool
        public let waiting: Int
        public let link: AlondraContract.Link?
    }

    public let title: String
    public let state: String?
    public let tasksTitle: String?
    public let tasks: [Entry]
    public let text: String?
    public let withdraw: WireAction?
}

public extension RequestDetail {
    init(document: ViewDocument) {
        func section(_ id: String) -> ViewSection? { document.sections.first { $0.id == id } }
        let facts = (section("facts")?.items ?? []).flatMap { item -> [Fact] in
            if case .facts(let facts) = item { facts.items } else { [] }
        }
        self.init(
            title: document.title ?? "",
            state: facts.first?.value,
            tasksTitle: section("tasks")?.title,
            tasks: (section("tasks")?.items ?? []).compactMap { item in
                guard case .row(let row) = item else { return nil }
                return Entry(
                    id: row.id, title: row.title, state: row.subtitle, tone: row.badges.first?.tone,
                    icon: row.icon, struck: row.struck, waiting: row.trailing?.count ?? 0, link: row.link
                )
            },
            text: (section("body")?.items ?? []).compactMap { item -> String? in
                if case .markdown(let markdown) = item, !markdown.text.isEmpty { markdown.text } else { nil }
            }.first,
            withdraw: (section("actions")?.items ?? []).flatMap { item -> [WireAction] in
                if case .actions(let bar) = item { bar.actions } else { [] }
            }.first { $0.command == "withdraw" }
        )
    }
}
