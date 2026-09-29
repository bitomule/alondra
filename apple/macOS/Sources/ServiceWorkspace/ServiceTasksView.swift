import AlondraWorkspaceFeature
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
                        .disabled(!store.board.canArchive || store.board.isArchiving)
                    ServiceTaskOutcome(outcome: store.board.archiveOutcome)
                }
                DisclosureGroup("Archived", isExpanded: Binding(get: { store.board.showArchived }, set: { store.send(.board(.showArchived($0))) })) {
                    ServiceRequestSection(title: "Archived requests", trees: store.archivedTrees) { store.send(.openTask($0)) }
                }
            }
            .navigationTitle("Tasks")
            .toolbar { Button("Schedules", systemImage: "calendar.badge.clock") { store.send(.showSchedules(true)) } }
            .sheet(isPresented: Binding(get: { store.showingSchedules }, set: { store.send(.showSchedules($0)) })) {
                ServiceSchedulesView(store: store).frame(minWidth: 600, minHeight: 520)
            }
            .navigationDestination(item: $store.scope(state: \.detail, action: \.detail)) { detailStore in
                ServiceTaskDetailView(store: detailStore, context: store.detailContext, openTask: { store.send(.openTask($0)) }, focusInbox: { store.send(.focusInbox($0)) })
            }
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .onChange(of: store.focusedInboxID) { _, id in if let id { focusInbox(id); store.send(.inboxFocusHandled) } }
    }
}

private struct ServiceRequestSection: View {
    let title: LocalizedStringKey
    let trees: [RequestTree]
    let open: (DomainV2.TaskId) -> Void
    var body: some View {
        Section(title) {
            ForEach(trees) { tree in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(tree.request.headline ?? String(localized: "Request")).font(.headline)
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
        case .unknown: Text("We couldn’t confirm the result. Try again; it won’t be applied twice.").foregroundStyle(.orange)
        case .rejected(let message): Text(message).foregroundStyle(.red)
        case .obsolete: Text("The connection changed. Open this task again.").foregroundStyle(.secondary)
        case .confirmed, nil: EmptyView()
        }
    }
}

struct ServiceTaskDetailView: View {
    @Bindable var store: StoreOf<TaskDetailFeature>
    let context: ServiceTaskContext
    let openTask: (DomainV2.TaskId) -> Void
    let focusInbox: (String) -> Void
    var body: some View {
        if let task = store.task {
            ServiceTaskPage {
                ServiceTaskHeader(task: task, context: context)
                if let now = context.now {
                    ServiceTaskSection(String(localized: "Right now")) {
                        Text(now).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, 12).padding(.vertical, 10)
                            .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                    }
                }
                if !context.pending.isEmpty {
                    ServiceTaskSection(String(localized: "Waiting for You")) {
                        VStack(alignment: .leading, spacing: 2) {
                            ForEach(context.pending) { item in
                                Button { focusInbox(item.id) } label: {
                                    HStack {
                                        InboxRow(item: item)
                                        Spacer()
                                        Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
                                    }.padding(.horizontal, 10).padding(.vertical, 4).contentShape(.rect)
                                }.buttonStyle(.plain)
                            }
                        }.background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                    }
                }
                if let request = context.request {
                    ServiceTaskRequestText(request: request, createdAt: context.requestCreatedAt, taskCount: context.requestTaskCount)
                }
                if !context.pullRequests.isEmpty {
                    ServiceTaskSection(String(localized: "Pull requests in merge order")) {
                        VStack(spacing: 0) {
                            ForEach(Array(context.pullRequests.enumerated()), id: \.element.id) { index, pr in
                                if index > 0 { Divider().padding(.horizontal, 12) }
                                if let owner = context.pullRequestOwners[pr.id] {
                                    ServiceTaskPullRequestRow(order: index + 1, pr: pr, observation: context.observations[pr.id], owner: owner,
                                        mine: owner.id == task.id, available: context.pullRequestItems[pr.id] != nil) {
                                            if let item = context.pullRequestItems[pr.id] { focusInbox(item) }
                                        }
                                }
                            }
                        }.background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                    }
                }
                ServiceTaskSection(String(localized: "Dependencies")) {
                    Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 14, verticalSpacing: 8) {
                        GridRow {
                            Text("Depends on").foregroundStyle(.secondary)
                            ServiceTaskLinks(tasks: context.dependencies, open: openTask)
                        }
                        GridRow {
                            Text("Waiting on it").foregroundStyle(.secondary)
                            ServiceTaskLinks(tasks: context.dependents, open: openTask)
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 12).padding(.vertical, 10)
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                }
                let reports = context.documents
                if !reports.isEmpty {
                    ServiceTaskSection(String(localized: "Documents")) {
                        VStack(spacing: 0) {
                            ForEach(Array(reports.enumerated()), id: \.element.id) { index, report in
                                if index > 0 { Divider().padding(.horizontal, 12) }
                                HStack(spacing: 10) {
                                    Image(systemName: "doc.text").foregroundStyle(.blue)
                                    VStack(alignment: .leading, spacing: 1) {
                                        Text(report.document.title).fontWeight(.medium)
                                        Text(context.documentItems[report.id] != nil ? String(localized: "unread") : String(localized: "read")).font(.caption).foregroundStyle(.secondary)
                                    }
                                    Spacer()
                                    if let item = context.documentItems[report.id] {
                                        Button("Read") { focusInbox(item) }.controlSize(.small)
                                    }
                                }.padding(.horizontal, 12).padding(.vertical, 8)
                            }
                        }.background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                    }
                }
                ServiceTaskSection(String(localized: "Options")) {
                    Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 8) {
                        GridRow {
                            Text("PR handling").foregroundStyle(.secondary)
                            Text(OptionSpec.prHandling(autoMerge: !task.reviewPrs))
                        }
                        if let effort = context.effort {
                            GridRow { Text("Effort").foregroundStyle(.secondary); Text(effort.capitalizedFirst) }
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 12).padding(.vertical, 10)
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                }
                if case let history = context.history, !history.isEmpty {
                    ServiceTaskSection(String(localized: "History")) {
                        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 10, verticalSpacing: 6) {
                            ForEach(Array(history.enumerated()), id: \.offset) { _, step in
                                GridRow {
                                    Text(Date(timeIntervalSince1970: TimeInterval(step.at) / 1000), format: .dateTime.hour().minute())
                                        .font(.system(.caption, design: .monospaced)).foregroundStyle(.tertiary)
                                    StatusDot(color: step.to.color)
                                    (Text(step.to.label).fontWeight(.semibold) + Text(verbatim: " · \(step.summary)").foregroundStyle(.secondary))
                                }
                            }
                        }
                    }
                }
                ServiceTaskOutcome(outcome: store.mutationOutcome)
            }
            .toolbar {
                ToolbarItemGroup {
                    Button { store.send(.reopenTapped) } label: { Label("Reopen", systemImage: "arrow.clockwise") }
                        .disabled(!task.state.isFinished || store.isActing)
                        .help("Reopen a finished or cancelled task")
                    Button(role: .destructive) { store.send(.cancelTapped) } label: { Label("Cancel Task…", systemImage: "xmark.circle") }
                        .disabled(task.state.isFinished || store.isActing)
                        .help("Its node stops working on it")
                }
            }
            .confirmationDialog("Cancel “\(task.title)”?", isPresented: Binding(get: { store.confirmation == .cancel }, set: { if !$0 { store.send(.cancelConfirmationDismissed) } })) {
                Button("Cancel Task", role: .destructive) { store.send(.cancelConfirmed) }
            } message: { Text("Its node stops working on it. You can reopen it later.") }
            .onDisappear { store.send(.disappeared) }
        } else { ContentUnavailableView("This task is gone", systemImage: "questionmark.circle") }
    }
}

private struct ServiceTaskHeader: View {
    let task: DomainV2.Task
    let context: ServiceTaskContext
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 14) {
                if let project = context.project { Label(project, systemImage: "folder") }
                if context.request != nil { Label(context.requestTitle ?? String(localized: "Request"), systemImage: "text.quote").lineLimit(1) }
            }.font(.callout).foregroundStyle(.secondary)
            Text(task.title).font(.title.weight(.semibold)).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                StatusDot(color: task.color, size: 9)
                Text(task.state.label.capitalizedFirst)
                if let since = context.stateSince { Text("· for \(ago(since))").foregroundStyle(.secondary) }
                if let node = context.agentName { Text("· \(node)").foregroundStyle(.secondary) }
            }.font(.callout)
        }
    }
}

private struct ServiceTaskLinks: View {
    let tasks: [DomainV2.Task]
    let open: (DomainV2.TaskId) -> Void
    var body: some View {
        if tasks.isEmpty { Text("none").foregroundStyle(.secondary) }
        else {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(tasks) { task in
                    Button { open(task.id) } label: {
                        HStack(spacing: 6) {
                            StatusDot(color: task.color)
                            Text(task.title)
                            Text(verbatim: "· \(task.state.label)").foregroundStyle(.secondary)
                        }.contentShape(.rect)
                    }.buttonStyle(.plain)
                }
            }
        }
    }
}

private struct ServiceTaskRequestText: View {
    let request: DomainV2.Request
    let createdAt: Int64?
    let taskCount: Int

    var body: some View {
        if case let text = request.body, !text.isEmpty {
            ServiceTaskSection(String(localized: "Request")) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(text)
                        .textSelection(.enabled)
                    Text(verbatim: [createdAt.map { String(localized: "\(ago($0)) ago") }, String(localized: "\(taskCount) tasks")].compactMap { $0 }.joined(separator: " · "))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
            }
        }
    }
}

private struct ServiceTaskPullRequestRow: View {
    let order: Int
    let pr: DomainV2.PullRequest
    let observation: PullRequest?
    let owner: DomainV2.Task
    let mine: Bool
    let available: Bool
    let open: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Text(order, format: .number)
                .font(.caption.weight(.semibold))
                .monospacedDigit()
                .frame(width: 18, height: 18)
                .background(Color.primary.opacity(0.08), in: .circle)
            Image(systemName: "arrow.triangle.pull")
                .foregroundStyle(.green)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) {
                    Text(verbatim: "#\(pr.number) \(observation?.title ?? pr.repository)")
                        .fontWeight(.medium)
                    if mine {
                        Text("this task")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.blue)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 1)
                            .background(.blue.opacity(0.14), in: .capsule)
                    }
                }
                let summary = observation?.summary ?? "\(pr.repository) #\(pr.number)"
                Text(verbatim: observation?.stackedOn.map { "\($0) · \(summary)" } ?? summary)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if let reason = observation?.mergeBlockedReason {
                Text(reason)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            } else {
                StatusDot(color: owner.color)
                if let observation {
                    Text(verbatim: observation.progress(mergesAlone: !owner.reviewPrs))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Button("Merge…", action: open)
                .controlSize(.small)
                .disabled(observation?.mergeBlockedReason != nil || !available)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }
}
