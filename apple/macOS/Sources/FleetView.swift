import AlondraShared
import AppKit
import SwiftUI

enum Selection: Hashable {
    case item(String)
    case node(String)
}

extension EnvironmentValues {
    @Entry var searchActive = false
}

public struct FleetView: View {
    @Environment(LocalDevice.self) private var device
    @State private var store = FleetStore()
    @State private var drafts = Drafts()
    @State private var selection: Selection?
    @State private var didAutoSelect = false
    @State private var query = ""
    @State private var smartMatches: Set<String>?
    @State private var smartStatus = ""
    @State private var showingNew = false
    @State private var closing: CloseRequest?
    @FocusState private var searchFocused: Bool
    @AppStorage("collapsedProjects") private var collapsedRaw = ""

    public init() {}

    /// While the user clicks, types or scrolls here, alondrad holds back iPhone pushes.
    private func reportActivity() async {
        let (events, continuation) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .leftMouseDown, .rightMouseDown, .scrollWheel]) { event in
            continuation.yield()
            return event
        }
        defer {
            if let monitor { NSEvent.removeMonitor(monitor) }
        }
        var told = Date.distantPast
        for await _ in events where Date().timeIntervalSince(told) >= 60 {
            told = Date()
            try? await store.client.activity(client: "mac")
        }
    }

    public var body: some View {
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
            .overlay(alignment: .top) { ErrorBanner(store: store) }
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
                    showingNew = true
                } label: {
                    Label("New", systemImage: "square.and.pencil")
                }
                .keyboardShortcut("n")
                .help("Ask for new work (⌘N)")
            }
        }
        .sheet(isPresented: $showingNew) {
            NewRequestSheet(store: store)
        }
        .closeConfirmation($closing) { request in
            guard let node = store.node(request.session) else { return }
            Task { await store.close(node) }
        }
        .closeRefusalAlert(store)
        .task {
            store.use(device.client)
            await store.run()
        }
        .task { await reportActivity() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            store.retryPendingSends()
        }
        .onChange(of: device.client.identity) { store.use(device.client) }
        .task(id: store.connection) { await pairIfNeeded() }
        .onChange(of: inboxIDs, initial: true) { old, new in
            follow(from: old, to: new)
        }
    }

    private var inboxIDs: [String] { store.inbox.map(\.id) }

    private func pairIfNeeded() async {
        guard store.connection == .unpaired || (store.connection == .online && !device.isPaired) else { return }
        while !Task.isCancelled {
            do {
                try await device.pair()
                return
            } catch where error.isCancellation {
                return
            } catch {
                store.lastError = String(localized: "Couldn’t pair this Mac with alondrad: \(error.friendlyDescription)")
            }
            try? await Task.sleep(for: .seconds(30))
        }
    }

    private var subtitle: String {
        guard store.connected, let overview = store.overview else { return String(localized: "No connection to alondrad") }
        let count = store.inbox.count
        let pending = count == 0 ? String(localized: "All caught up") : String(localized: "\(count) pending")
        let alive = store.nodes.filter(\.alive).count
        return [pending, String(localized: "\(alive) active nodes")].joined(separator: " · ")
    }

    private var sidebar: some View {
        List(selection: $selection) {
            if !smartStatus.isEmpty {
                Label(smartStatus, systemImage: "text.magnifyingglass")
                    .font(.callout)
                    .foregroundStyle(.secondary)
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
            ForEach(filteredProjects, id: \.name) { project in
                Section(isExpanded: expansion(for: project.name)) {
                    ForEach(project.nodes) { node in
                        NodeRow(node: node)
                            .badge(store.pendingCount(for: node.sessionId))
                            .tag(Selection.node(node.sessionId))
                            .contextMenu {
                                Button(role: .destructive) {
                                    closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .node)
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
        .searchable(text: $query, placement: .sidebar, prompt: "Search")
        .searchFocused($searchFocused)
        .task(id: query) { await runSmartSearch() }
    }

    @ViewBuilder
    private var detail: some View {
        switch selection {
        case .item(let id):
            if let item = store.item(id: id) {
                ItemDetail(item: item, store: store, drafts: drafts)
            } else {
                HomeView(store: store, start: startTriage, newRequest: { showingNew = true })
            }
        case .node(let id):
            if let node = store.node(id) {
                NodeDetail(node: node, store: store, drafts: drafts, open: { selection = .item($0) })
            } else {
                ContentUnavailableView("This node is gone", systemImage: "questionmark.circle")
            }
        case nil:
            HomeView(store: store, start: startTriage, newRequest: { showingNew = true })
        }
    }

    private func startTriage() {
        selection = store.inbox.first.map { .item($0.id) }
    }

    private func follow(from old: [String], to new: [String]) {
        if !didAutoSelect, selection == nil, let first = new.first {
            didAutoSelect = true
            selection = .item(first)
            return
        }
        guard case .item(let id)? = selection, !new.contains(id) else { return }
        let remaining = Set(new)
        guard let index = old.firstIndex(of: id) else {
            selection = new.first.map { .item($0) }
            return
        }
        let next = old[(index + 1)...].first(where: remaining.contains) ?? old[..<index].last(where: remaining.contains)
        selection = next.map { .item($0) }
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
                let haystack = "\(node.displayName) \(project.name) \(node.stateLabel) \(node.summary)".lowercased()
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
        guard !Task.isCancelled, filteredProjects.isEmpty, visibleInbox.isEmpty else { return }
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
            } else if !store.stuckSends.isEmpty {
                HStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text("Couldn’t send \(store.stuckSends.count): Alondra didn’t answer for 2 minutes").lineLimit(2)
                    Button {
                        store.dismissStuckSends()
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
            } else if !store.pendingSends.isEmpty {
                Label(String(localized: "Alondra isn’t responding; \(store.pendingSends.count) pending, retrying"), systemImage: "arrow.clockwise")
                    .lineLimit(1)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 9)
                    .background(.regularMaterial, in: .capsule)
                    .padding(.top, 10)
                    .transition(.opacity)
            }
        }
        .animation(.snappy, value: store.lastError)
        .animation(.snappy, value: store.pendingSends.count)
        .animation(.snappy, value: store.stuckSends)
    }
}
