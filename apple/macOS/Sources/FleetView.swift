import AlondraShared
import SwiftUI

enum Selection: Hashable {
    case item(String)
    case node(String)
}

extension EnvironmentValues {
    @Entry var searchActive = false
}

public struct FleetView: View {
    @State private var store = FleetStore()
    @State private var drafts = Drafts()
    @State private var selection: Selection?
    @State private var didAutoSelect = false
    @State private var query = ""
    @State private var smartMatches: Set<String>?
    @State private var smartStatus = ""
    @State private var showingNew = false
    @FocusState private var searchFocused: Bool
    @AppStorage("collapsedProjects") private var collapsedRaw = ""

    public init() {}

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
            ToolbarItem(placement: .primaryAction) {
                Button {
                    showingNew = true
                } label: {
                    Label("Nuevo", systemImage: "square.and.pencil")
                }
                .keyboardShortcut("n")
                .help("Pedir trabajo nuevo (⌘N)")
            }
        }
        .sheet(isPresented: $showingNew) {
            NewRequestSheet(store: store)
        }
        .task { await store.run() }
        .onChange(of: inboxIDs, initial: true) { old, new in
            follow(from: old, to: new)
        }
    }

    private var inboxIDs: [String] { store.inbox.map(\.id) }

    private var subtitle: String {
        guard store.connected, let overview = store.overview else { return "Sin conexión con alondrad" }
        let pending = switch store.inbox.count {
        case 0: "Todo al día"
        case 1: "1 pendiente"
        case let count: "\(count) pendientes"
        }
        let machine = overview.machine
        let memory = machine.memoryFreePercent.map { "memoria \($0) % libre" } ?? "\(machine.memoryAvailable / 1_073_741_824) GB libres"
        let load = "carga \(Int((machine.load.first ?? 0).rounded()))"
        let alive = store.nodes.filter(\.alive).count
        return [pending, "\(alive) nodos activos", memory, load + (machine.unreliable ? " ⚠︎" : "")].joined(separator: " · ")
    }

    private var sidebar: some View {
        List(selection: $selection) {
            if !smartStatus.isEmpty {
                Label(smartStatus, systemImage: "text.magnifyingglass")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Section {
                if visibleInbox.isEmpty {
                    Label(query.isEmpty ? "Todo al día" : "Nada coincide", systemImage: query.isEmpty ? "checkmark.circle" : "magnifyingglass")
                        .foregroundStyle(.secondary)
                }
                ForEach(visibleInbox) { item in
                    InboxRow(item: item)
                        .tag(Selection.item(item.id))
                }
            } header: {
                HStack(spacing: 6) {
                    Text("Te espera")
                    if !store.inbox.isEmpty {
                        Text("\(store.inbox.count)")
                            .monospacedDigit()
                            .foregroundStyle(.white)
                            .padding(.horizontal, 6)
                            .background(Color.orange, in: .capsule)
                    }
                }
            }
            ForEach(filteredProjects, id: \.name) { project in
                Section(isExpanded: expansion(for: project.name)) {
                    ForEach(project.nodes) { node in
                        NodeRow(node: node)
                                .badge(store.pendingCount(for: node.sessionId))
                            .tag(Selection.node(node.sessionId))
                    }
                } header: {
                    ProjectHeader(name: project.name, nodes: project.nodes)
                }
            }
        }
        .animation(.snappy, value: inboxIDs)
        .searchable(text: $query, placement: .sidebar, prompt: "Buscar")
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
                ContentUnavailableView("Este nodo ya no está", systemImage: "questionmark.circle")
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
                let haystack = "\(node.displayName) \(project.name) \(node.state.label) \(node.summary)".lowercased()
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
        smartStatus = "Buscando en las conversaciones…"
        let result = await store.smartSearch(q)
        guard !Task.isCancelled else { return }
        if let result {
            smartMatches = Set(result.sessions)
            smartStatus = result.sessions.isEmpty ? "Tampoco aparece en las conversaciones" : "Encontrado en las conversaciones"
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
                    Label("Sin conexión con alondrad", systemImage: "bolt.horizontal.circle")
                } description: {
                    Text("Reintento cada pocos segundos.")
                } actions: {
                    ProgressView().controlSize(.small)
                }
            }
        } else if store.inbox.isEmpty {
            ContentUnavailableView {
                Label("Todo al día", systemImage: "checkmark.circle")
            } description: {
                Text("Ningún nodo te necesita ahora mismo. Lo que llegue aparecerá en «Te espera».")
            } actions: {
                Button("Pedir algo nuevo…", action: newRequest)
            }
        } else {
            ContentUnavailableView {
                Label(store.inbox.count == 1 ? "1 cosa te espera" : "\(store.inbox.count) cosas te esperan", systemImage: "tray.full")
            } description: {
                Text("Respondes, y pasa sola a la siguiente.")
            } actions: {
                Button("Empezar", action: start)
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
                    .accessibilityLabel("Cerrar")
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
            }
        }
        .animation(.snappy, value: store.lastError)
    }
}
