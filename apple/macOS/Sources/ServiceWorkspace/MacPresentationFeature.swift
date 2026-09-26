import AlondraShared
import ComposableArchitecture

/// Per-window navigation and disclosure state. The service graph remains the sole
/// owner of snapshots, drafts and mutations.
@Reducer
struct MacPresentationFeature {
    enum Selection: Hashable {
        case item(String), node(String), task(String), request(String), schedules
    }
    @ObservableState
    struct State: Equatable {
        var selection: Selection?
        var query = ""
        var tasksExpanded = true
        var collapsedProjects: Set<String> = []
        var toggledTaskGroups: Set<String> = []
        var newRequestPresented = false

        func projectExpanded(_ name: String) -> Bool { !query.isEmpty || !collapsedProjects.contains(name) }
        func groupExpanded(_ group: TaskStateGroup) -> Bool {
            !query.isEmpty || (!group.isFinished != toggledTaskGroups.contains(String(describing: group.kind)))
        }
        func matches(_ text: String) -> Bool {
            let haystack = text.lowercased()
            return query.lowercased().split(separator: " ").allSatisfy { haystack.contains($0) }
        }
        func groups(in board: TaskBoard) -> [TaskStateGroup] {
            board.groups.compactMap { group in
                let filtered = TaskStateGroup(kind: group.kind,
                    tasks: group.tasks.filter { matches($0.title) },
                    requests: group.requests.filter { matches($0.title ?? $0.headline ?? "") })
                return filtered.count == 0 ? nil : filtered
            }
        }
        func inbox(_ items: [InboxItem], pullRequests: Bool) -> [InboxItem] {
            items.filter { $0.isPullRequest == pullRequests && matches("\($0.title) \($0.nodeName ?? "")") }
        }
    }
    enum Action {
        case select(Selection?)
        case queryChanged(String)
        case tasksExpanded(Bool)
        case projectExpanded(String, Bool)
        case groupExpanded(String, defaultExpanded: Bool, expanded: Bool)
        case newRequestPresented(Bool)
    }
    var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .select(let selection): state.selection = selection
            case .queryChanged(let query): state.query = query
            case .tasksExpanded(let expanded): state.tasksExpanded = expanded
            case .projectExpanded(let name, let expanded):
                if expanded { state.collapsedProjects.remove(name) } else { state.collapsedProjects.insert(name) }
            case .groupExpanded(let key, let defaultExpanded, let expanded):
                if expanded == defaultExpanded { state.toggledTaskGroups.remove(key) } else { state.toggledTaskGroups.insert(key) }
            case .newRequestPresented(let presented): state.newRequestPresented = presented
            }
            return .none
        }
    }
}
