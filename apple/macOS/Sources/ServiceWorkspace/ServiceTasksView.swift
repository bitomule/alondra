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
                if let text = context.request?.text, !text.isEmpty {
                    ServiceTaskSection(String(localized: "Request")) { MarkdownText(text: text) }
                }
                if !context.pullRequests.isEmpty {
                    ServiceTaskSection(String(localized: "Pull requests in merge order")) {
                        VStack(spacing: 0) {
                            ForEach(Array(context.pullRequests.enumerated()), id: \.element.id) { index, pr in
                                if index > 0 { Divider().padding(.horizontal, 12) }
                                Button { focusInbox(InboxItem.pullRequest(pr).id) } label: {
                                    HStack(spacing: 10) {
                                        Text("\(index + 1)").font(.caption.monospaced()).foregroundStyle(.secondary)
                                        InboxRow(item: .pullRequest(pr))
                                        Spacer()
                                        Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
                                    }.padding(.horizontal, 12).padding(.vertical, 8).contentShape(.rect)
                                }.buttonStyle(.plain)
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
                let reports = store.detail?.documents ?? context.reports
                if !reports.isEmpty {
                    ServiceTaskSection(String(localized: "Documents")) {
                        VStack(spacing: 0) {
                            ForEach(Array(reports.enumerated()), id: \.element.id) { index, report in
                                if index > 0 { Divider().padding(.horizontal, 12) }
                                HStack(spacing: 10) {
                                    Image(systemName: "doc.text").foregroundStyle(.blue)
                                    VStack(alignment: .leading, spacing: 1) {
                                        Text(report.title).fontWeight(.medium)
                                        Text("\(report.kindLabel) · \(ago(report.createdAt)) ago").font(.caption).foregroundStyle(.secondary)
                                    }
                                    Spacer()
                                    if context.pending.contains(where: { $0.id == "r\(report.id)" }) {
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
                if let project = context.request?.options?.project { Label(project, systemImage: "folder") }
                if let request = context.request { Label(request.title ?? request.headline ?? "Request", systemImage: "text.quote").lineLimit(1) }
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
