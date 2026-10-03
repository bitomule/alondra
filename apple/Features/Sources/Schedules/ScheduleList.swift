import AlondraContract
import Foundation

/// The `schedules` view: every scheduled task with what you can do to it.
public struct ScheduleList: Equatable, Sendable {
    public struct Entry: Equatable, Sendable, Identifiable {
        public let id: String
        public let title: String
        public let frequency: String?
        public let badges: [AlondraContract.Label]
        public let nextRun: Date?
        public let link: AlondraContract.Link?
        public let run: WireAction?
        public let toggle: WireAction?
        public let delete: WireAction?

        public var isPaused: Bool { badges.contains { $0.tone == .warning } }
    }

    public let title: String
    public let entries: [Entry]
    public let empty: Notice?
    public let add: ScreenDelegate?
}

public extension ScheduleList {
    init(document: ViewDocument) {
        func items(_ id: String) -> [ViewItem] { document.sections.first { $0.id == id }?.items ?? [] }
        let add = items("actions").lazy.flatMap { item -> [WireAction] in
            if case .actions(let bar) = item { bar.actions } else { [] }
        }.compactMap(\.link).compactMap { ScreenDelegate.opening($0) }.first
        self.init(
            title: document.title ?? "",
            entries: items("schedules").compactMap { item in
                guard case .row(let row) = item else { return nil }
                return Entry(
                    id: row.id, title: row.title, frequency: row.subtitle, badges: row.badges,
                    nextRun: row.trailing?.instant, link: row.link,
                    run: row.actions.first { $0.command == "sched_run" },
                    toggle: row.actions.first { $0.command == "sched_toggle" },
                    delete: row.actions.first { $0.command == "sched_delete" }
                )
            },
            empty: document.empty,
            add: add
        )
    }
}

public extension ScheduleList {
    /// The list without the schedules just deleted.
    func hiding(_ optimistic: Optimistic) -> ScheduleList {
        guard !optimistic.isEmpty else { return self }
        return ScheduleList(title: title, entries: entries.filter { !optimistic.hides($0.delete?.id) }, empty: empty, add: add)
    }
}
