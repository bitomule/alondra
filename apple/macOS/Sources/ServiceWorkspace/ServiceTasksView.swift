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

struct ServiceTaskDetailView: View {
    @Bindable var store: StoreOf<TaskDetailFeature>
    let context: ServiceTaskContext
    let openTask: (String) -> Void
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
                    ServiceTaskRequestText(request: request, taskCount: context.requestTaskCount)
                }
                if !context.pullRequests.isEmpty {
                    ServiceTaskSection(String(localized: "Pull requests in merge order")) {
                        VStack(spacing: 0) {
                            ForEach(Array(context.pullRequests.enumerated()), id: \.element.id) { index, pr in
                                if index > 0 { Divider().padding(.horizontal, 12) }
                                if let owner = context.pullRequestOwners[pr.id] {
                                    ServiceTaskPullRequestRow(order: index + 1, pr: pr, owner: owner,
                                        mine: owner.id == task.id, available: context.inboxIDs.contains("p\(pr.id)")) {
                                            focusInbox(InboxItem.pullRequest(pr).id)
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
                let reports = context.documents(in: store.detail)
                if !reports.isEmpty {
                    ServiceTaskSection(String(localized: "Documents")) {
                        VStack(spacing: 0) {
                            ForEach(Array(reports.enumerated()), id: \.element.id) { index, report in
                                if index > 0 { Divider().padding(.horizontal, 12) }
                                HStack(spacing: 10) {
                                    Image(systemName: "doc.text").foregroundStyle(.blue)
                                    VStack(alignment: .leading, spacing: 1) {
                                        Text(report.title).fontWeight(.medium)
                                        Text("\(report.kindLabel) · \(context.unreadReportIDs.contains(report.id) ? String(localized: "unread") : String(localized: "read")) · \(ago(report.createdAt)) ago").font(.caption).foregroundStyle(.secondary)
                                    }
                                    Spacer()
                                    if context.unreadReportIDs.contains(report.id) {
                                        Button("Read") { focusInbox("r\(report.id)") }.controlSize(.small)
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
                            Text(OptionSpec.prHandling(autoMerge: task.options?.mergesAlone ?? false))
                        }
                        if let effort = task.options?.effort {
                            GridRow { Text("Effort").foregroundStyle(.secondary); Text(effort.capitalizedFirst) }
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 12).padding(.vertical, 10)
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                }
                if let history = store.detail?.task.history, !history.isEmpty {
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
                        .disabled(!task.state.isFinished || store.isActing || store.mutationOutcome?.uncertainID != nil)
                        .help("Reopen a finished or cancelled task")
                    Button(role: .destructive) { store.send(.cancelTapped) } label: { Label("Cancel Task…", systemImage: "xmark.circle") }
                        .disabled(task.state.isFinished || store.isActing || store.mutationOutcome?.uncertainID != nil)
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
    let task: WorkTask
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
                Text("· for \(ago(task.stateSince))").foregroundStyle(.secondary)
                if let node = context.nodeName { Text("· \(node)").foregroundStyle(.secondary) }
            }.font(.callout)
        }
    }
}

private struct ServiceTaskLinks: View {
    let tasks: [WorkTask]
    let open: (String) -> Void
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
    let request: WorkRequest
    let taskCount: Int

    var body: some View {
        if let text = request.text, !text.isEmpty {
            ServiceTaskSection(String(localized: "Request")) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(text)
                        .textSelection(.enabled)
                    Text(verbatim: "\(String(localized: "\(ago(request.createdAt)) ago")) · \(String(localized: "\(taskCount) tasks"))")
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
    let pr: PullRequest
    let owner: WorkTask
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
                    Text(verbatim: "#\(pr.number) \(pr.title)")
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
                Text(verbatim: pr.stackedOn.map { "\($0) · \(pr.summary)" } ?? pr.summary)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if let reason = pr.mergeBlockedReason {
                Text(reason)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            } else {
                StatusDot(color: owner.color)
                Text(verbatim: pr.progress(mergesAlone: owner.mergesAlone))
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Button("Merge…", action: open)
                .controlSize(.small)
                .disabled(pr.mergeBlockedReason != nil || !available)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }
}
