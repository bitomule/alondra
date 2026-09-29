import AlondraFleetFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI

struct MacServiceWorkspace<AccountContent: View>: View {
    @Bindable var store: StoreOf<ServiceRootFeature>
    let accountContent: AccountContent
    var isActive = true
    private var presentation: StoreOf<MacPresentationFeature> { store.scope(state: \.macPresentation, action: \.macPresentation) }
    @FocusState private var searchFocused: Bool

    init(store: StoreOf<ServiceRootFeature>, accountContent: AccountContent, isActive: Bool = true) {
        self.store = store
        self.accountContent = accountContent
        self.isActive = isActive
    }

    var body: some View {
        NavigationSplitView {
            MacServiceSidebar(store: store, presentation: presentation)
                .navigationSplitViewColumnWidth(min: 260, ideal: 320, max: 440)
                .searchable(text: Binding(get: { presentation.query }, set: {
                    presentation.send(.queryChanged($0))
                    presentation.send(.searchRequested(inbox: store.triage.triage.queue, board: store.tasks.taskBoard, fleet: store.fleet.fleet.snapshot))
                }), placement: .sidebar, prompt: "Search")
                .searchFocused($searchFocused)
        } detail: {
            ZStack {
                MacServiceDetail(store: store, presentation: presentation)
                    .id(presentation.selection)
                    .transition(.asymmetric(insertion: .opacity.combined(with: .offset(y: 6)), removal: .opacity))
            }
            .animation(.smooth(duration: 0.22), value: presentation.selection)
            .overlay(alignment: .top) { ServiceDeliveryBanner(store: store) }
        }
        .environment(\.searchActive, searchFocused)
        .environment(\.yourTurnTasks, store.workspace?.yourTurnTasks ?? [])
        .navigationTitle("Alondra")
        .navigationSubtitle(store.macSubtitle)
        .toolbar {
            if store.triage.triage.connection == .online, let machine = store.workspace?.runtime?.machine {
                ToolbarItem(placement: .status) { StatusGauges(machine: machine, usage: store.workspace?.runtime?.usage) }
                    .sharedBackgroundVisibility(.hidden)
            }
            ToolbarItemGroup(placement: .primaryAction) {
                Button { presentation.send(.newRequestPresented(true)) } label: { Label("New", systemImage: "square.and.pencil") }
                    .keyboardShortcut("n").help("Ask for new work (⌘N)")
                Button { store.send(.tasks(.board(.archiveTapped))) } label: { Label("Archive Completed", systemImage: "archivebox") }
                    .disabled(!store.tasks.board.canArchive || store.tasks.board.isArchiving)
                    .help("Archive completed tasks")
            }
        }
        .focusedSceneValue(\.workspaceSettingsStore, isActive ? store : nil)
        .sheet(isPresented: Binding(get: { presentation.newRequestPresented }, set: { presentation.send(.newRequestPresented($0)) })) {
            ServiceNewRequestView(store: store.scope(state: \.newRequest, action: \.newRequest))
        }
        .sheet(isPresented: Binding(get: { store.settingsPresented }, set: { store.send(.settingsPresented($0)) })) {
            accountContent.frame(minWidth: 600, minHeight: 520)
        }
        .task { guard isActive else { return }; await store.send(.appeared).finish() }
        .task { guard isActive else { return }; await store.send(.tasks(.appeared)).finish() }
        .task { guard isActive else { return }; await store.send(.fleet(.appeared)).finish() }
        .onDisappear {
            presentation.send(.disappeared)
            guard isActive else { return }
            store.send(.disappeared)
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
        .onChange(of: store.triage.triage.queue.map(\.id), initial: true) { _, ids in
            presentation.send(.inboxChanged(ids))
        }
        .frame(minWidth: 860, minHeight: 540)
    }
}

private struct MacServiceSidebar: View {
    let store: StoreOf<ServiceRootFeature>
    let presentation: StoreOf<MacPresentationFeature>
    var body: some View {
        List(selection: Binding(get: { presentation.selection }, set: { presentation.send(.select($0)) })) {
            if !presentation.smartStatus.isEmpty {
                Label(presentation.smartStatus, systemImage: "text.magnifyingglass").font(.callout).foregroundStyle(.secondary)
            }
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
                            MacServiceTaskRow(task: task, board: store.tasks.taskBoard, subtitle: store.tasks.presentation.sidebarSubtitle(for: task)).tag(MacPresentationFeature.Selection.task(task.id))
                        }
                        ForEach(group.requests) { request in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(store.tasks.presentation.title(of: request)).fontWeight(.medium).lineLimit(2)
                                Text(verbatim: store.tasks.presentation.sidebarSubtitle(for: request)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }.padding(.vertical, 2).accessibilityElement(children: .combine).tag(MacPresentationFeature.Selection.request(request.id))
                        }
                    } label: { TaskGroupLabel(group: group) }
                }
            } header: { SidebarCountHeader(title: String(localized: "Tasks"), count: store.tasks.taskBoard.openCount, tint: .gray) }
            ForEach(presentation.withState { $0.projects(store.fleet.fleet.snapshot) }) { section in
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
        .closeConfirmation(Binding(get: { store.fleet.fleet.closeRequest }, set: { if $0 == nil { store.send(.fleet(.fleet(.closeCancelled))) } })) { _ in
            store.send(.fleet(.fleet(.closeConfirmed)))
        }
        .closeRefusalAlert(Binding(get: { store.fleet.fleet.closeRefusal }, set: { if $0 == nil { store.send(.fleet(.fleet(.dismissCloseRefusal))) } }))
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
                    let context = store.withState { $0.rowContext(item) }
                    InboxRow(item: item, agent: context.agent, date: context.date, stackedOn: context.stackedOn,
                             asks: store.withState { $0.asksOnPullRequest(item) }).tag(MacPresentationFeature.Selection.item(item.id))
                }
            } header: {
                SidebarCountHeader(title: pullRequests ? String(localized: "Pull Requests") : String(localized: "Waiting for You"), count: store.withState { $0.inboxCount(pullRequests: pullRequests) }, tint: pullRequests ? .purple : .orange)
            }
        }
    }
}

private struct MacServiceTaskRow: View {
    let task: DomainV2.Task
    let board: TaskBoard
    let subtitle: String
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(task.title).fontWeight(.medium).strikethrough(task.state == .cancelled).lineLimit(2)
            Text(verbatim: subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
        }.padding(.vertical, 2).accessibilityElement(children: .combine).accessibilityValue(task.state.label)
    }
}

private struct MacServiceDetail: View {
    @Bindable var store: StoreOf<ServiceRootFeature>
    let presentation: StoreOf<MacPresentationFeature>
    var body: some View {
        switch presentation.selection {
        case .item(let id):
            if let selected = store.pullRequestID, id == ServiceRootFeature.State.itemID(of: selected),
               let child = store.scope(state: \.pullRequests, action: \.pullRequests).first(where: { $0.id == selected }) {
                ServicePullRequestView(store: child, taskTitle: store.withState { $0.taskTitle(forInboxID: id) })
            } else if let child = store.scope(state: \.triage.question, action: \.triage.question.presented), id == child.question.item.id {
                ServiceQuestionView(store: child, taskTitle: store.withState { $0.taskTitle(forInboxID: id) })
            } else if let child = store.scope(state: \.triage.documents, action: \.triage.documents).first(where: { $0.subject.id == id }) {
                ServiceDocumentView(store: child, taskTitle: store.withState { $0.taskTitle(forInboxID: id) }, markRead: { store.send(.triage(.triage(.hide(id)))) })
            } else { MacServiceHome(store: store, presentation: presentation) }
        case .node(let id):
            if let child = store.scope(state: \.fleet.nodes, action: \.fleet.nodes).first(where: { $0.sessionID == id }) {
                ServiceNodeView(store: child, presentation: store.fleet.presentation,
                    pending: store.triage.triage.queue.filter { $0.agentId?.rawValue == child.sessionID },
                    openInbox: { presentation.send(.select(.item($0))) },
                    openTask: { presentation.send(.select(.task($0))) })
            } else { ContentUnavailableView("This node is gone", systemImage: "questionmark.circle") }
        case .task:
            if let child = store.scope(state: \.tasks.detail, action: \.tasks.detail.presented) {
                ServiceTaskDetailView(store: child, context: store.tasks.detailContext,
                    openTask: { presentation.send(.select(.task($0))) }, focusInbox: { presentation.send(.select(.item($0))) })
            } else { ContentUnavailableView("This task is gone", systemImage: "questionmark.circle") }
        case .request(let id):
            if let request = store.tasks.taskBoard.request(id) {
                ServiceRequestDetail(request: request, store: store.scope(state: \.tasks, action: \.tasks),
                    selectTask: { presentation.send(.select(.task($0))) })
            } else { ContentUnavailableView("This request is gone", systemImage: "questionmark.circle") }
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
            ProgressView("Connecting to the fleet…")
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
