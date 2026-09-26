import AlondraShared
import AlondraTasksFeature
import ComposableArchitecture
import SwiftUI

struct ServiceTasksView: View {
    @Bindable var store: StoreOf<ServiceTasksFeature>
    var focusInbox: (String) -> Void
    var body: some View {
        NavigationStack {
            List {
                if !store.board.hasSnapshot { ProgressView("Loading tasks") }
                ServiceRequestSection(title: "Open", trees: store.openTrees) { store.send(.openTask($0)) }
                DisclosureGroup("Done", isExpanded: Binding(get: { store.board.showDone }, set: { store.send(.board(.showDone($0))) })) {
                    ServiceRequestSection(title: "Completed requests", trees: store.doneTrees) { store.send(.openTask($0)) }
                    Button("Archive completed tasks") { store.send(.board(.archiveTapped)) }
                        .disabled(!store.board.canArchive || store.board.isArchiving || store.board.archiveOutcome?.uncertainID != nil)
                    ServiceTaskOutcome(outcome: store.board.archiveOutcome)
                }
                DisclosureGroup("Archived", isExpanded: Binding(get: { store.board.showArchived }, set: { store.send(.board(.showArchived($0))) })) {
                    ServiceRequestSection(title: "Archived requests", trees: store.archivedTrees) { store.send(.openTask($0)) }
                }
            }
            .navigationTitle("Tasks")
            .toolbar { Button("Schedules", systemImage: "calendar.badge.clock") { store.send(.showSchedules(true)) } }
            .sheet(isPresented: Binding(get: { store.showingSchedules }, set: { store.send(.showSchedules($0)) })) {
                ServiceSchedulesView(store: store)
            }
            .navigationDestination(item: $store.scope(state: \.detail, action: \.detail)) { detailStore in
                ServiceTaskDetailView(store: detailStore, context: store.detailContext, openTask: { store.send(.openTask($0)) }, focusInbox: { store.send(.focusInbox($0)) })
            }
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .onChange(of: store.focusedInboxID) { _, id in if let id { focusInbox(id) } }
    }
}

private struct ServiceRequestSection: View {
    let title: LocalizedStringKey
    let trees: [RequestTree]
    let open: (String) -> Void
    var body: some View {
        Section(title) {
            ForEach(trees) { tree in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(tree.request.title ?? tree.request.headline ?? "Request").font(.headline)
                        ForEach(tree.rows, id: \.task.id) { row in
                            Button { open(row.task.id) } label: {
                                HStack {
                                    Text(row.task.title)
                                    Spacer()
                                    Text(row.task.state.label).font(.caption).foregroundStyle(.secondary)
                                }.padding(.leading, CGFloat(row.depth) * 12)
                            }
                        }
                    }
            }
        }
    }
}

struct ServiceTaskOutcome: View {
    let outcome: TaskMutationOutcome?
    var body: some View {
        switch outcome {
        case .unknown: Text("The response was lost. This action is blocked until its outcome is reconciled.").foregroundStyle(.orange)
        case .rejected(let message): Text(message).foregroundStyle(.red)
        case .obsolete: Text("The connection changed. Open this task again.").foregroundStyle(.secondary)
        case .confirmed, nil: EmptyView()
        }
    }
}

private struct ServiceTaskDetailView: View {
    @Bindable var store: StoreOf<TaskDetailFeature>
    let context: ServiceTaskContext
    let openTask: (String) -> Void
    let focusInbox: (String) -> Void
    var body: some View {
        List {
            if let task = store.task {
                Section {
                    Text(task.title).font(.title2.bold())
                    Text(task.state.label)
                    if let node = context.nodeName { Text(node).foregroundStyle(.secondary) }
                }
                if !context.pending.isEmpty {
                    Section("Waiting for You") {
                        ForEach(context.pending) { item in Button(item.title) { focusInbox(item.id) } }
                    }
                }
                if let text = context.request?.text { Section("Request") { Text(text).textSelection(.enabled) } }
                ServiceTaskDependencies(title: "Depends on", tasks: context.dependencies, open: openTask)
                ServiceTaskDependencies(title: "Waiting on it", tasks: context.dependents, open: openTask)
                Section("PRs in merge order") {
                    ForEach(context.pullRequests) { pr in Button(pr.title) { focusInbox(InboxItem.pullRequest(pr).id) } }
                }
                Section("Documents") {
                    ForEach(store.detail?.documents ?? context.reports) { report in
                        NavigationLink(report.title) { ScrollView { Text(report.body ?? "").textSelection(.enabled).padding() }.navigationTitle(report.title) }
                    }
                }
                Section("Options") {
                    LabeledContent("PR handling", value: OptionSpec.prHandling(autoMerge: task.options?.mergesAlone ?? false))
                    if let effort = task.options?.effort { LabeledContent("Effort", value: effort) }
                }
                Section("History") {
                    ForEach(Array((store.detail?.task.history ?? []).enumerated()), id: \.offset) { _, step in
                        VStack(alignment: .leading) {
                            Text(step.to.label).font(.headline)
                            Text(step.summary)
                            Text(Date(timeIntervalSince1970: Double(step.at) / 1000), format: .dateTime).font(.caption)
                        }
                    }
                }
                Section {
                    Button("Cancel Task", role: .destructive) { store.send(.cancelTapped) }.disabled(task.state.isFinished || store.isActing || store.mutationOutcome?.uncertainID != nil)
                    Button("Reopen") { store.send(.reopenTapped) }.disabled(!task.state.isFinished || store.isActing || store.mutationOutcome?.uncertainID != nil)
                    ServiceTaskOutcome(outcome: store.mutationOutcome)
                }
            } else { ContentUnavailableView("This task is gone", systemImage: "questionmark.circle") }
        }
        .navigationTitle("Task")
        .confirmationDialog("Cancel this task?", isPresented: Binding(get: { store.confirmation == .cancel }, set: { if !$0 { store.send(.cancelConfirmationDismissed) } })) {
            Button("Cancel Task", role: .destructive) { store.send(.cancelConfirmed) }
        }
        .onDisappear { store.send(.disappeared) }
    }
}
private struct ServiceTaskDependencies: View {
    let title: LocalizedStringKey
    let tasks: [WorkTask]
    let open: (String) -> Void
    var body: some View {
        Section(title) {
            if tasks.isEmpty { Text("None").foregroundStyle(.secondary) }
            ForEach(tasks) { task in Button(task.title) { open(task.id) } }
        }
    }
}
