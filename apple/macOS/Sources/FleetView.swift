import AlondraShared
import AlondraMacFeature
import ComposableArchitecture
import SwiftUI

extension EnvironmentValues {
    @Entry var searchActive = false
}

struct FleetView: View {
    @Environment(LocalDevice.self) private var device
    let store: FleetStore
    @Bindable var feature: StoreOf<MacWindowFeature>
    let services: MacAppSync
    let drafts: Drafts
    @State private var smartMatches: Set<String>?
    @State private var smartStatus = ""
    @State private var closing: CloseRequest?
    @FocusState private var searchFocused: Bool
    @AppStorage("collapsedProjects") private var collapsedRaw = ""
    @AppStorage("tasksExpanded") private var tasksExpanded = true
    @AppStorage("toggledTaskGroups") private var toggledGroupsRaw = ""

    private var selection: Selection? { feature.selection }
    private var query: String { feature.query }
    private var newRequestIsPresented: Binding<Bool> {
        Binding(
            get: { feature.isNewRequestPresented },
            set: { if !$0 { feature.send(.dismissNewRequest) } }
        )
    }

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 260, ideal: 320, max: 440)
        } detail: {
            ZStack {
                detail
                    .id(selection)
                    .transition(.asymmetric(insertion: .opacity.combined(with: .offset(y: 6)), removal: .opacity))
            }
            .animation(.smooth(duration: 0.22), value: selection)
            .overlay(alignment: .top) {
                ErrorBanner(
                    store: store,
                    pairingError: feature.pairingError,
                    dismissPairingError: { feature.send(.dismissPairingError) }
                )
            }
        }
        .environment(\.searchActive, searchFocused)
        .navigationTitle("Alondra")
        .navigationSubtitle(subtitle)
        .toolbar {
            if store.connected, let overview = store.overview {
                ToolbarItem(placement: .status) {
                    StatusGauges(machine: overview.machine, usage: overview.usage)
                }
                .sharedBackgroundVisibility(.hidden)
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    feature.send(.showNewRequest)
                } label: {
                    Label("New", systemImage: "square.and.pencil")
                }
                .keyboardShortcut("n")
                .help("Ask for new work (⌘N)")
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    services.runMutation { await store.archiveCompleted() }
                } label: {
                    Label("Archive Completed", systemImage: "archivebox")
                }
                .disabled(!store.canArchive)
                .help("Archive every done or cancelled task")
            }
        }
        .sheet(isPresented: newRequestIsPresented) {
            NewRequestSheet(
                store: store,
                submit: MacNewRequestSubmission(
                    feature: feature,
                    fleet: store,
                    services: services,
                    origin: feature.newRequestOrigin,
                    originEpoch: feature.newRequestOriginEpoch,
                    originScope: services.commandScope
                ).call
            )
        }
        .closeConfirmation($closing) { request in
            guard let node = store.node(request.session) else { return }
            services.runMutation { await store.close(node) }
        }
        .closeRefusalAlert(store)
        .task(id: store.connection) { await services.pairIfNeeded(device: device, connection: store.connection) }
    }

    private var inboxIDs: [String] { store.inbox.map(\.id) }

    private var subtitle: String {
        guard store.connected, store.overview != nil else { return String(localized: "No connection to alondrad") }
        let count = store.inbox.count
        let pending = count == 0 ? String(localized: "All caught up") : String(localized: "\(count) pending")
        return [pending, String(localized: "\(store.nodes.count) nodes")].joined(separator: " · ")
    }

    private var sidebar: some View {
        List(selection: $feature.selection) {
            if !smartStatus.isEmpty {
                Label(smartStatus, systemImage: "text.magnifyingglass")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            // The orchestrator's own fixed area, first and outside every project folder: a
            // state change of the orchestrator (or of a regular node) must never move it.
            if let orchestrator = store.orchestrator {
                Section {
                    NodeRow(node: orchestrator)
                        .tag(Selection.node(orchestrator.sessionId))
                } header: {
                    SidebarCountHeader(title: String(localized: "Orchestrator"), count: 0, tint: .orange)
                }
            }
            Section {
                if waiting.isEmpty {
                    Label(query.isEmpty ? String(localized: "All caught up") : String(localized: "Nothing matches"), systemImage: query.isEmpty ? "checkmark.circle" : "magnifyingglass")
                        .foregroundStyle(.secondary)
                }
                ForEach(waiting) { item in
                    InboxRow(item: item)
                        .tag(Selection.item(item.id))
                }
            } header: {
                SidebarCountHeader(title: String(localized: "Waiting for You"), count: store.inbox.count(where: { !$0.isPullRequest }), tint: .orange)
            }
            if !pullRequests.isEmpty {
                Section {
                    ForEach(pullRequests) { item in
                        InboxRow(item: item, asks: !store.questions(for: item).isEmpty)
                            .tag(Selection.item(item.id))
                    }
                } header: {
                    SidebarCountHeader(title: String(localized: "Pull Requests"), count: store.inbox.count(where: \.isPullRequest), tint: .purple)
                }
            }
            Section(isExpanded: $tasksExpanded) {
                Label("Scheduled Tasks", systemImage: "calendar.badge.clock")
                    .tag(Selection.schedules)
                ForEach(taskGroups) { group in
                    DisclosureGroup(isExpanded: expansion(for: group)) {
                        ForEach(group.tasks) { task in
                            TaskRow(task: task, store: store)
                                .tag(Selection.task(task.id))
                        }
                        ForEach(group.requests) { request in
                            DefiningRow(request: request, store: store)
                                .tag(Selection.request(request.id))
                        }
                    } label: {
                        TaskGroupLabel(group: group)
                    }
                }
            } header: {
                SidebarCountHeader(title: String(localized: "Tasks"), count: store.board.openCount, tint: .gray)
            }
            ForEach(filteredProjects, id: \.name) { project in
                Section(isExpanded: expansion(for: project.name)) {
                    ForEach(project.nodes) { node in
                        NodeRow(node: node, tasks: store.board.tasks(onNode: node.sessionId))
                            .badge(store.pendingCount(for: node.sessionId))
                            .tag(Selection.node(node.sessionId))
                            .contextMenu {
                                Button(role: .destructive) {
                                    closing = CloseRequest(session: node.sessionId, name: node.displayName)
                                } label: {
                                    Label("Close Node…", systemImage: "xmark.bin")
                                }
                            }
                    }
                } header: {
                    ProjectHeader(name: project.name, nodes: project.nodes)
                }
            }
        }
        .animation(.snappy, value: inboxIDs)
        .searchable(text: $feature.query, placement: .sidebar, prompt: "Search")
        .searchFocused($searchFocused)
        .task(id: query) { await runSmartSearch() }
    }

    @ViewBuilder
    private var detail: some View {
        switch selection {
        case .schedules:
            ScheduledTasksView(client: store.client)
        case .item(let id):
            if let item = store.item(id: id) {
                ItemDetail(item: item, store: store, drafts: drafts)
            } else {
                HomeView(store: store, start: startTriage, newRequest: { feature.send(.showNewRequest) })
            }
        case .node(let id):
            if let node = store.node(id) {
                NodeDetail(node: node, store: store, drafts: drafts, open: { feature.selection = .item($0) }, openTask: { feature.selection = .task($0) })
            } else {
                ContentUnavailableView("This node is gone", systemImage: "questionmark.circle")
            }
        case .task(let id):
            if let task = store.board.task(id) {
                TaskDetail(task: task, store: store, select: { feature.selection = $0 })
            } else {
                ContentUnavailableView("This task is gone", systemImage: "questionmark.circle")
            }
        case .request(let id):
            if let request = store.board.request(id) {
                RequestDetail(request: request, store: store, select: { feature.selection = $0 })
            } else {
                ContentUnavailableView("This request is gone", systemImage: "questionmark.circle")
            }
        case nil:
            HomeView(store: store, start: startTriage, newRequest: { feature.send(.showNewRequest) })
        }
    }

    private func startTriage() {
        feature.send(.startTriage)
    }

    private var collapsed: Set<String> {
        Set(collapsedRaw.split(separator: "\n").map(String.init))
    }

    private func expansion(for project: String) -> Binding<Bool> {
        Binding {
            !query.isEmpty || !collapsed.contains(project)
        } set: { expanded in
            var set = collapsed
            if expanded { set.remove(project) } else { set.insert(project) }
            collapsedRaw = set.sorted().joined(separator: "\n")
        }
    }

    /// What waits for the user first; finished groups start folded.
    private func expansion(for group: TaskStateGroup) -> Binding<Bool> {
        let key = "\(group.kind)"
        let byDefault = !group.isFinished
        return Binding {
            !terms.isEmpty || byDefault != toggledGroups.contains(key)
        } set: { expanded in
            var set = toggledGroups
            if expanded == byDefault { set.remove(key) } else { set.insert(key) }
            toggledGroupsRaw = set.sorted().joined(separator: "\n")
        }
    }

    private var toggledGroups: Set<String> {
        Set(toggledGroupsRaw.split(separator: "\n").map(String.init))
    }

    private var taskGroups: [TaskStateGroup] {
        let groups = store.board.groups
        guard !terms.isEmpty else { return groups }
        return groups.compactMap { group in
            let tasks = group.tasks.filter { task in terms.allSatisfy { task.title.lowercased().contains($0) } }
            let requests = group.requests.filter { request in terms.allSatisfy { store.title(of: request).lowercased().contains($0) } }
            let kept = TaskStateGroup(kind: group.kind, tasks: tasks, requests: requests)
            return kept.count == 0 ? nil : kept
        }
    }

    private var terms: [Substring] {
        query.lowercased().split(separator: " ")
    }

    private var waiting: [InboxItem] { visibleInbox.filter { !$0.isPullRequest } }
    private var pullRequests: [InboxItem] { visibleInbox.filter(\.isPullRequest) }

    private var visibleInbox: [InboxItem] {
        guard !terms.isEmpty else { return store.inbox }
        return store.inbox.filter { item in
            let haystack = "\(item.title) \(item.nodeName ?? "")".lowercased()
            return terms.allSatisfy { haystack.contains($0) }
        }
    }

    private var filteredProjects: [(name: String, nodes: [Node])] {
        guard !terms.isEmpty else { return store.projects }
        return store.projects.compactMap { project in
            let nodes = project.nodes.filter { node in
                if let smartMatches { return smartMatches.contains(node.sessionId) }
                let tasks = store.board.tasks(onNode: node.sessionId).map(\.title).joined(separator: " ")
                let haystack = "\(node.displayName) \(project.name) \(node.fleetLabel) \(node.summary) \(tasks)".lowercased()
                return terms.allSatisfy { haystack.contains($0) }
            }
            return nodes.isEmpty ? nil : (name: project.name, nodes: nodes)
        }
    }

    private func runSmartSearch() async {
        smartMatches = nil
        smartStatus = ""
        let q = query.trimmingCharacters(in: .whitespaces)
        guard q.count >= 3 else { return }
        try? await Task.sleep(for: .milliseconds(1200))
        guard !Task.isCancelled, filteredProjects.isEmpty, visibleInbox.isEmpty, taskGroups.isEmpty else { return }
        smartStatus = String(localized: "Searching the conversations…")
        let result = await store.smartSearch(q)
        guard !Task.isCancelled else { return }
        if let result {
            smartMatches = Set(result.sessions)
            smartStatus = result.sessions.isEmpty ? String(localized: "Not in the conversations either") : String(localized: "Found in the conversations")
        } else {
            smartStatus = ""
        }
    }
}

extension FleetStore {
    func item(id: String) -> InboxItem? {
        if id.hasPrefix("q") {
            return questions.first { "q\($0.id)" == id }.map(InboxItem.question)
        }
        if id.hasPrefix("p") {
            return (overview?.prs ?? []).first { "p\($0.id)" == id }.map(InboxItem.pullRequest)
        }
        return news.first { "r\($0.id)" == id }.map(InboxItem.report)
    }
}

struct HomeView: View {
    let store: FleetStore
    let start: () -> Void
    let newRequest: () -> Void

    var body: some View {
        if store.overview == nil {
            if store.connected {
                ProgressView()
            } else {
                ContentUnavailableView {
                    Label("No connection to alondrad", systemImage: "bolt.horizontal.circle")
                } description: {
                    Text("Retrying every few seconds.")
                } actions: {
                    ProgressView().controlSize(.small)
                }
            }
        } else if store.inbox.isEmpty {
            ContentUnavailableView {
                Label("All caught up", systemImage: "checkmark.circle")
            } description: {
                Text("No node needs you right now. Anything new will show up in Waiting for You.")
            } actions: {
                Button("Ask for Something New…", action: newRequest)
            }
        } else {
            ContentUnavailableView {
                Label("\(store.inbox.count) things waiting for you", systemImage: "tray.full")
            } description: {
                Text("Answer, and it moves on to the next one by itself.")
            } actions: {
                Button("Start", action: start)
                    .keyboardShortcut(.defaultAction)
                    .controlSize(.large)
            }
        }
    }
}

struct ErrorBanner: View {
    let store: FleetStore
    let pairingError: String?
    let dismissPairingError: () -> Void

    var body: some View {
        ZStack {
            if let message = store.lastError {
                HStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text(message).lineLimit(2)
                    Button {
                        store.lastError = nil
                    } label: {
                        Image(systemName: "xmark").font(.caption.weight(.bold))
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel("Close")
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(.regularMaterial, in: .capsule)
                .shadow(color: .black.opacity(0.12), radius: 8, y: 2)
                .padding(.top, 10)
                .transition(.move(edge: .top).combined(with: .opacity))
                .task(id: message) {
                    try? await Task.sleep(for: .seconds(6))
                    if !Task.isCancelled { store.lastError = nil }
                }
            } else if let pairingError {
                HStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text(pairingError).lineLimit(2)
                    Button(action: dismissPairingError) {
                        Image(systemName: "xmark").font(.caption.weight(.bold))
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel("Close")
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(.regularMaterial, in: .capsule)
                .padding(.top, 10)
                .transition(.opacity)
            } else if !store.uncertainSends.isEmpty {
                HStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text("Check Alondra: \(store.uncertainActionDescription) may have completed").lineLimit(2)
                    Button {
                        Task { await store.dismissUncertainSends() }
                    } label: {
                        Image(systemName: "xmark").font(.caption.weight(.bold))
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel("Close")
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(.regularMaterial, in: .capsule)
                .padding(.top, 10)
                .transition(.opacity)
            }
        }
        .animation(.snappy, value: store.lastError)
        .animation(.snappy, value: store.uncertainSends)
    }
}
