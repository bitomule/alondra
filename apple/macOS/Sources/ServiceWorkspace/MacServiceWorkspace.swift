import AlondraFleetFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI

struct MacServiceWorkspace<AccountContent: View>: View {
    @Bindable var store: StoreOf<ServiceRootFeature>
    let accountContent: AccountContent
    var isActive = true
    @State private var presentation = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
    @FocusState private var searchFocused: Bool

    var body: some View {
        NavigationSplitView {
            MacServiceSidebar(store: store, presentation: presentation)
                .navigationSplitViewColumnWidth(min: 260, ideal: 320, max: 440)
                .searchable(text: Binding(get: { presentation.query }, set: { presentation.send(.queryChanged($0)); store.send(.fleet(.fleet(.searchChanged($0)))) }), placement: .sidebar, prompt: "Search")
                .searchFocused($searchFocused)
        } detail: {
            ZStack {
                MacServiceDetail(store: store, presentation: presentation)
                    .id(presentation.selection)
                    .transition(.asymmetric(insertion: .opacity.combined(with: .offset(y: 6)), removal: .opacity))
            }
            .animation(.smooth(duration: 0.22), value: presentation.selection)
            .overlay(alignment: .top) {
                if let notice = store.triage.triage.notice {
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                        Text(notice)
                        Button { store.send(.triage(.triage(.dismissNotice))) } label: { Image(systemName: "xmark") }
                            .buttonStyle(.borderless).accessibilityLabel("Close")
                    }.padding(10).background(.regularMaterial, in: .capsule).padding(.top, 8)
                }
            }
        }
        .environment(\.searchActive, searchFocused)
        .navigationTitle("Alondra")
        .navigationSubtitle(store.triage.triage.queue.isEmpty ? "All caught up" : "\(store.triage.triage.queue.count) pending · \(store.fleet.overview?.nodes.count ?? 0) nodes")
        .toolbar {
            if let overview = store.fleet.overview {
                ToolbarItem(placement: .status) { StatusGauges(machine: overview.machine, usage: overview.usage) }
                    .sharedBackgroundVisibility(.hidden)
            }
            ToolbarItemGroup(placement: .primaryAction) {
                Button { presentation.send(.newRequestPresented(true)) } label: { Label("New…", systemImage: "square.and.pencil") }
                    .keyboardShortcut("n", modifiers: [.command, .shift]).help("Ask for new work (⇧⌘N)")
                Button { store.send(.tasks(.board(.archiveTapped))) } label: { Label("Archive Completed", systemImage: "archivebox") }
                    .disabled(!store.tasks.board.canArchive || store.tasks.board.isArchiving || store.tasks.board.archiveOutcome?.uncertainID != nil)
                    .help("Archive completed tasks")
                Button { store.send(.settingsPresented(true)) } label: { Label("Settings", systemImage: "gearshape") }
                    .keyboardShortcut(",", modifiers: .command)
            }
        }
        .sheet(isPresented: Binding(get: { presentation.newRequestPresented }, set: { presentation.send(.newRequestPresented($0)) })) {
            ServiceNewRequestView(store: store.scope(state: \.newRequest, action: \.newRequest)).frame(minWidth: 600, minHeight: 520)
        }
        .sheet(isPresented: Binding(get: { store.settingsPresented }, set: { store.send(.settingsPresented($0)) })) {
            accountContent.frame(minWidth: 600, minHeight: 520)
        }
        .task { guard isActive else { return }; await store.send(.appeared).finish() }
        .task { guard isActive else { return }; await store.send(.triage(.triage(.appeared))).finish() }
        .task { guard isActive else { return }; await store.send(.tasks(.appeared)).finish() }
        .task { guard isActive else { return }; await store.send(.fleet(.appeared)).finish() }
        .onDisappear {
            guard isActive else { return }
            store.send(.disappeared)
            store.send(.triage(.triage(.disappeared)))
            store.send(.tasks(.disappeared))
            store.send(.fleet(.disappeared))
        }
        .onChange(of: presentation.selection) { _, selection in
            switch selection {
            case .item(let id): store.send(.focusInbox(id))
            case .node(let id): store.send(.fleet(.openNode(id)))
            case .task(let id): store.send(.tasks(.openTask(id)))
            case .request, .schedules, nil: break
            }
        }
        .onChange(of: store.triage.triage.queue.map(\.id)) { old, new in
            guard case .item(let id) = presentation.selection, old.contains(id), !new.contains(id) else { return }
            presentation.send(.select(new.first.map(MacPresentationFeature.Selection.item)))
        }
        .frame(minWidth: 740, minHeight: 520)
    }
}

private struct MacServiceSidebar: View {
    let store: StoreOf<ServiceRootFeature>
    let presentation: StoreOf<MacPresentationFeature>
    var body: some View {
        List(selection: Binding(get: { presentation.selection }, set: { presentation.send(.select($0)) })) {
            if let entry = store.fleet.fleet.snapshot?.orchestrator {
                Section {
                    NodeRow(node: entry.node).tag(MacPresentationFeature.Selection.node(entry.id))
                } header: { SidebarCountHeader(title: String(localized: "Orchestrator"), count: 0, tint: .orange) }
            }
            MacServiceInboxSection(store: store, presentation: presentation, pullRequests: false)
            MacServiceInboxSection(store: store, presentation: presentation, pullRequests: true)
            Section(isExpanded: Binding(get: { presentation.tasksExpanded }, set: { presentation.send(.tasksExpanded($0)) })) {
                Label("Scheduled Tasks", systemImage: "calendar.badge.clock").tag(MacPresentationFeature.Selection.schedules)
                ForEach(presentation.withState { $0.groups(in: store.tasks.taskBoard) }) { group in
                    DisclosureGroup(isExpanded: Binding(get: { presentation.withState { $0.groupExpanded(group) } }, set: { presentation.send(.groupExpanded(String(describing: group.kind), defaultExpanded: !group.isFinished, expanded: $0)) })) {
                        ForEach(group.tasks) { task in
                            MacServiceTaskRow(task: task, board: store.tasks.taskBoard).tag(MacPresentationFeature.Selection.task(task.id))
                        }
                        ForEach(group.requests) { request in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(request.title ?? request.headline ?? "Request").fontWeight(.medium).lineLimit(2)
                                Text(request.state == .done ? "finished without tasks" : "the orchestrator is splitting it").font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }.padding(.vertical, 2).tag(MacPresentationFeature.Selection.request(request.id))
                        }
                    } label: { TaskGroupLabel(group: group) }
                }
            } header: { SidebarCountHeader(title: String(localized: "Tasks"), count: store.tasks.taskBoard.openCount, tint: .gray) }
            ForEach(store.fleet.fleet.filteredSections) { section in
                Section(isExpanded: Binding(get: { presentation.withState { $0.projectExpanded(section.name) } }, set: { presentation.send(.projectExpanded(section.name, $0)) })) {
                    ForEach(section.nodes) { entry in
                        NodeRow(node: entry.node, tasks: entry.tasks).badge(entry.pendingCount)
                            .tag(MacPresentationFeature.Selection.node(entry.id))
                            .contextMenu {
                                Button(role: .destructive) { store.send(.fleet(.fleet(.closeRequested(entry.id)))) } label: { Label("Close Node…", systemImage: "xmark.bin") }
                            }
                    }
                } header: { ProjectHeader(name: section.name, nodes: section.nodes.map(\.node)) }
            }
        }
        .animation(.snappy, value: store.triage.triage.queue.map(\.id))
        .confirmationDialog("Close this node?", isPresented: Binding(get: { store.fleet.fleet.closeConfirmationID != nil }, set: { if !$0 { store.send(.fleet(.fleet(.closeCancelled))) } })) {
            Button("Close Node", role: .destructive) { store.send(.fleet(.fleet(.closeConfirmed))) }
        }
    }
}

private struct MacServiceInboxSection: View {
    let store: StoreOf<ServiceRootFeature>
    let presentation: StoreOf<MacPresentationFeature>
    let pullRequests: Bool
    var body: some View {
        let items = presentation.withState { $0.inbox(store.triage.triage.queue, pullRequests: pullRequests) }
        if !pullRequests || !items.isEmpty {
            Section {
                if items.isEmpty {
                    Label(presentation.query.isEmpty ? "All caught up" : "Nothing matches", systemImage: presentation.query.isEmpty ? "checkmark.circle" : "magnifyingglass").foregroundStyle(.secondary)
                }
                ForEach(items) { item in
                    InboxRow(item: item).tag(MacPresentationFeature.Selection.item(item.id))
                        .contextMenu {
                            Button("Skip for Now") { store.send(.triage(.triage(.skip(item.id)))) }
                            if case .report = item { Button("Mark as Read") { store.send(.triage(.triage(.hide(item.id)))) } }
                            if case .pullRequest = item { Button("Dismiss Pull Request") { store.send(.triage(.triage(.hide(item.id)))) } }
                        }
                }
            } header: {
                SidebarCountHeader(title: pullRequests ? String(localized: "Pull Requests") : String(localized: "Waiting for You"), count: items.count, tint: pullRequests ? .purple : .orange)
            }
        }
    }
}

private struct MacServiceTaskRow: View {
    let task: WorkTask
    let board: TaskBoard
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(board.rowTitle(of: task)).fontWeight(.medium).strikethrough(task.state == .cancelled).lineLimit(2)
            Text(board.stateLabel(of: task)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
        }.padding(.vertical, 2).accessibilityElement(children: .combine).accessibilityValue(task.state.label)
    }
}

private struct MacServiceDetail: View {
    @Bindable var store: StoreOf<ServiceRootFeature>
    let presentation: StoreOf<MacPresentationFeature>
    var body: some View {
        switch presentation.selection {
        case .item(let id):
            if let child = store.scope(state: \.pullRequest, action: \.pullRequest.presented), id == "p\(child.pr.id)" {
                ServicePullRequestView(store: child)
            } else if let child = store.scope(state: \.triage.question, action: \.triage.question.presented), id == "q\(child.question.id)" {
                ServiceQuestionView(store: child)
            } else if let child = store.scope(state: \.triage.document, action: \.triage.document.presented), child.subject.id == id {
                ServiceDocumentView(store: child, markRead: { store.send(.triage(.triage(.hide(id)))) })
            } else { MacServiceHome(store: store, presentation: presentation) }
        case .node:
            if let child = store.scope(state: \.fleet.detail, action: \.fleet.detail.presented) { ServiceNodeView(store: child) }
        case .task:
            if let child = store.scope(state: \.tasks.detail, action: \.tasks.detail.presented) {
                ServiceTaskDetailView(store: child, context: store.tasks.detailContext,
                    openTask: { presentation.send(.select(.task($0))) }, focusInbox: { presentation.send(.select(.item($0))) })
            }
        case .request(let id):
            if let request = store.tasks.taskBoard.request(id) {
                ServiceTaskPage {
                    Text(request.title ?? request.headline ?? "Request").font(.title.weight(.semibold))
                    if let text = request.text { MarkdownText(text: text) }
                }
            }
        case .schedules: ServiceSchedulesView(store: store.scope(state: \.tasks, action: \.tasks))
        case nil: MacServiceHome(store: store, presentation: presentation)
        }
    }
}

private struct MacServiceHome: View {
    let store: StoreOf<ServiceRootFeature>
    let presentation: StoreOf<MacPresentationFeature>
    var body: some View {
        if !store.triage.triage.hasSnapshot {
            ProgressView()
        } else if store.triage.triage.queue.isEmpty {
            ContentUnavailableView {
                Label("All caught up", systemImage: "checkmark.circle")
            } description: {
                Text("No node needs you right now. Anything new will show up in Waiting for You.")
            } actions: {
                Button("Ask for Something New…") { presentation.send(.newRequestPresented(true)) }
            }
        } else {
            ContentUnavailableView {
                Label("\(store.triage.triage.queue.count) things waiting for you", systemImage: "tray.full")
            } description: {
                Text("Answer, and it moves on to the next one by itself.")
            } actions: {
                Button("Start") { presentation.send(.select(store.triage.triage.queue.first.map { .item($0.id) })) }
                    .keyboardShortcut(.defaultAction).controlSize(.large)
            }
        }
    }
}
