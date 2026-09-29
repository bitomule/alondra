import AlondraContracts
import AlondraDomain
import AlondraFleetFeature
import AlondraShared
import Foundation
import ComposableArchitecture

/// Per-window navigation and disclosure state. The service graph remains the sole
/// owner of snapshots, drafts and mutations.
@Reducer
public struct MacPresentationFeature {
    public init() {}
    public enum Selection: Hashable {
        case item(String), node(String), task(DomainV2.TaskId), request(DomainV2.RequestId), schedules
    }
    @ObservableState
    public struct State: Equatable {
        public init() {}
        public var selection: Selection?
        public var query = ""
        @Shared(.appStorage("tasksExpanded")) public var tasksExpanded = true
        @Shared(.appStorage("collapsedProjects")) public var collapsedProjectsRaw = ""
        @Shared(.appStorage("toggledTaskGroups")) public var toggledTaskGroupsRaw = ""
        public var collapsedProjects: Set<String> { Set(collapsedProjectsRaw.split(separator: "\n").map(String.init)) }
        public var toggledTaskGroups: Set<String> { Set(toggledTaskGroupsRaw.split(separator: "\n").map(String.init)) }
        public var newRequestPresented = false
        public var inboxIDs: [String] = []
        public var didAutoSelect = false
        public var smartMatches: Set<String>?
        public var smartStatus = ""
        public var searchRevision: UInt64 = 0

        public func projects(_ snapshot: FleetSnapshot?) -> [MacSidebarProject] {
            (snapshot?.sections ?? []).compactMap { section in
                let nodes = section.nodes.filter { entry in
                    if let smartMatches { return smartMatches.contains(entry.id) }
                    return matches(([entry.node.displayName, section.name, entry.node.fleetLabel, entry.node.summary] + entry.tasks.map(\.title)).joined(separator: " "))
                }
                return nodes.isEmpty ? nil : MacSidebarProject(name: section.name, nodes: nodes)
            }
        }

        private var terms: [Substring] { query.lowercased().split(separator: " ") }
        public func projectExpanded(_ name: String) -> Bool { !query.isEmpty || !collapsedProjects.contains(name) }
        public func groupExpanded(_ group: TaskStateGroup) -> Bool {
            !terms.isEmpty || (!group.isFinished != toggledTaskGroups.contains(String(describing: group.kind)))
        }
        public func matches(_ text: String) -> Bool {
            let haystack = text.lowercased()
            return terms.allSatisfy { haystack.contains($0) }
        }
        public func groups(in board: TaskBoard) -> [TaskStateGroup] {
            board.groups.compactMap { group in
                let filtered = TaskStateGroup(kind: group.kind, tasks: group.tasks.filter { task in
                    matches(task.title) || board.request(task.requestId).map { matches(ServiceTaskPresentation.title(of: $0, in: board)) } == true
                })
                return filtered.count == 0 ? nil : filtered
            }
        }
        public func inbox(_ items: [DomainV2.InboxItem], pullRequests: Bool) -> [DomainV2.InboxItem] {
            items.filter { $0.isPullRequest == pullRequests && matches("\($0.title) \($0.body ?? "")") }
        }
    }
    public enum Action {
        case select(Selection?)
        case inboxChanged([String])
        case queryChanged(String)
        case searchRequested(inbox: [DomainV2.InboxItem], board: TaskBoard, fleet: FleetSnapshot?)
        case searchStarted(UInt64)
        case searchFinished(UInt64, SearchResult?)
        case disappeared
        case tasksExpanded(Bool)
        case projectExpanded(String, Bool)
        case groupExpanded(String, defaultExpanded: Bool, expanded: Bool)
        case newRequestPresented(Bool)
    }
    @Dependency(\.macConversationSearch) var search
    @Dependency(\.continuousClock) var clock
    private enum CancelID { case search }
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .select(let selection): state.selection = selection
            case .inboxChanged(let ids):
                if !state.didAutoSelect, state.selection == nil, let first = ids.first {
                    state.didAutoSelect = true
                    state.selection = .item(first)
                } else if case .item(let id) = state.selection, !ids.contains(id) {
                    if let index = state.inboxIDs.firstIndex(of: id) {
                        let remaining = Set(ids)
                        let next = state.inboxIDs[(index + 1)...].first(where: remaining.contains)
                            ?? state.inboxIDs[..<index].last(where: remaining.contains)
                        state.selection = next.map(Selection.item)
                    } else { state.selection = ids.first.map(Selection.item) }
                }
                state.inboxIDs = ids
            case .queryChanged(let query):
                state.query = query
                state.smartMatches = nil
                state.smartStatus = ""
                state.searchRevision &+= 1
                return .cancel(id: CancelID.search)
            case .searchRequested(let inbox, let board, let fleet):
                let query = state.query.trimmingCharacters(in: .whitespaces)
                guard query.count >= 3, state.projects(fleet).isEmpty,
                      state.inbox(inbox, pullRequests: false).isEmpty,
                      state.inbox(inbox, pullRequests: true).isEmpty,
                      state.groups(in: board).isEmpty else { return .none }
                let revision = state.searchRevision
                return .run { send in
                    try await clock.sleep(for: .milliseconds(1200))
                    await send(.searchStarted(revision))
                    let result = try? await search.run(query)
                    try Task.checkCancellation()
                    await send(.searchFinished(revision, result))
                }.cancellable(id: CancelID.search, cancelInFlight: true)
            case .searchStarted(let revision):
                guard revision == state.searchRevision else { return .none }
                state.smartStatus = String(localized: "Searching the conversations…")
            case .searchFinished(let revision, let result):
                guard revision == state.searchRevision else { return .none }
                state.smartMatches = result.map { Set($0.sessions) }
                state.smartStatus = result.map { $0.sessions.isEmpty ? String(localized: "Not in the conversations either") : String(localized: "Found in the conversations") } ?? ""
            case .disappeared:
                state.searchRevision &+= 1
                return .cancel(id: CancelID.search)
            case .tasksExpanded(let expanded): state.$tasksExpanded.withLock { $0 = expanded }
            case .projectExpanded(let name, let expanded):
                state.$collapsedProjectsRaw.withLock { raw in
                    var collapsed = Set(raw.split(separator: "\n").map(String.init))
                    if expanded { collapsed.remove(name) } else { collapsed.insert(name) }
                    raw = collapsed.sorted().joined(separator: "\n")
                }
            case .groupExpanded(let key, let defaultExpanded, let expanded):
                state.$toggledTaskGroupsRaw.withLock { raw in
                    var toggled = Set(raw.split(separator: "\n").map(String.init))
                    if expanded == defaultExpanded { toggled.remove(key) } else { toggled.insert(key) }
                    raw = toggled.sorted().joined(separator: "\n")
                }
            case .newRequestPresented(let presented): state.newRequestPresented = presented
            }
            return .none
        }
    }
}

public struct MacSidebarProject: Identifiable {
    public var id: String { name }
    public let name: String
    public let nodes: [FleetNode]
}

public struct MacConversationSearch: Sendable {
    public var run: @Sendable (String) async throws -> SearchResult?
    public init(run: @escaping @Sendable (String) async throws -> SearchResult?) { self.run = run }
    public static let unavailable = Self(run: { _ in nil })
}
private enum MacConversationSearchKey: DependencyKey {
    static let liveValue = MacConversationSearch.unavailable
    static let testValue = liveValue
}
public extension DependencyValues {
    var macConversationSearch: MacConversationSearch {
        get { self[MacConversationSearchKey.self] }
        set { self[MacConversationSearchKey.self] = newValue }
    }
}
