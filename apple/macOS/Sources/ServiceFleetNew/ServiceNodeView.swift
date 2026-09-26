import AlondraAttachmentsFeature
import AlondraFleetFeature
import AlondraWorkspaceFeature
import AlondraShared
import ComposableArchitecture
import SwiftUI

struct ServiceNodeView: View {
    let store: StoreOf<NodeDetailFeature>
    var presentation = ServiceTaskPresentation(overview: nil)
    var pending: [InboxItem] = []
    var openInbox: (String) -> Void = { _ in }
    var openTask: (String) -> Void = { _ in }
    var body: some View {
        if let node = store.node {
            DetailScaffold {
                VStack(alignment: .leading, spacing: 22) {
                    ServiceNodeHeader(node: node)
                    if !pending.isEmpty {
                        ServiceTaskSection(String(localized: "Waiting for You")) {
                            VStack(alignment: .leading, spacing: 2) {
                                ForEach(pending) { item in
                                    Button { openInbox(item.id) } label: {
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
                    if !node.isOrchestrator && !store.tasks.isEmpty {
                        ServiceTaskSection(String(localized: "Tasks")) {
                            VStack(alignment: .leading, spacing: 2) {
                                ForEach(store.tasks) { task in
                                    Button { openTask(task.id) } label: {
                                        HStack(spacing: 10) {
                                            StatusDot(color: task.color)
                                            VStack(alignment: .leading, spacing: 1) {
                                                Text(task.title).fontWeight(.medium).strikethrough(task.state == .cancelled)
                                                Text(presentation.taskLine(task)).font(.caption).foregroundStyle(.secondary)
                                            }
                                            Spacer()
                                            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
                                        }.padding(.horizontal, 12).padding(.vertical, 7).contentShape(.rect)
                                    }.buttonStyle(.plain)
                                }
                            }.background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                        }
                    }
                    if !store.threads.isEmpty {
                        ServiceTaskSection(String(localized: "Right now")) {
                            VStack(alignment: .leading, spacing: 2) {
                                ForEach(store.threads, id: \.self) { activity in
                                    ServiceThreadRow(activity: activity, task: store.tasks.first { $0.id == activity.task })
                                }
                            }.background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                        }
                    }
                    if let error = store.threadsError { Text(error).font(.callout).foregroundStyle(.red) }
                    if !node.running.isEmpty {
                        ServiceTaskSection(BackgroundItem.sectionTitle) {
                            VStack(alignment: .leading, spacing: 6) {
                                ForEach(node.running, id: \.self) { BackgroundRow(item: $0) }
                            }
                        }
                    }
                    if let needs = node.daemonNeeds, !needs.isEmpty {
                        ServiceTaskSection(String(localized: "What it needs")) { Text(needs).textSelection(.enabled) }
                    }
                    if let last = node.lastMessage ?? node.daemonDetail, !last.isEmpty {
                        ServiceTaskSection(node.lastMessageAt.map { String(localized: "What it said last · \(ago($0)) ago") } ?? String(localized: "What it said last")) {
                            MarkdownText(text: last)
                        }
                    }
                    if let cwd = node.cwd {
                        Label(cwd, systemImage: "folder").font(.caption).foregroundStyle(.tertiary).textSelection(.enabled)
                    }
                }
            } bar: {
                VStack(alignment: .leading, spacing: 8) {
                    ServiceNodeComposer(store: store, name: node.displayName)
                    ServiceNodeOutcome(outcome: store.sendOutcome)
                    ServiceNodeOutcome(outcome: store.closeOutcome)
                }
            }
            .toolbar {
                ToolbarItem {
                    Button(role: .destructive) { store.send(.closeRequested) } label: { Label("Close Node…", systemImage: "xmark.bin") }
                        .disabled(store.isStale || store.isClosing || store.isSendInFlight || store.closeOutcome?.uncertainID != nil || store.sendOutcome?.uncertainID != nil)
                        .help("Close this node with claude rm")
                }
            }
            .alert("Close \(node.displayName)?", isPresented: Binding(get: { store.showCloseConfirmation }, set: { if !$0 { store.send(.closeCancelled) } })) {
                Button("Close Node", role: .destructive) { store.send(.closeConfirmed) }
                Button("Cancel", role: .cancel) { store.send(.closeCancelled) }
            } message: {
                Text("Runs claude rm. It never discards work: if the node has uncommitted or unpushed changes, or its worktree is still there, the server refuses and tells you why.")
            }
            .task { await store.send(.appeared).finish() }
            .onDisappear { store.send(.disappeared) }
        } else {
            ContentUnavailableView("This node is gone", systemImage: "questionmark.circle")
        }
    }
}

private struct ServiceNodeHeader: View {
    let node: Node
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Label(node.projectName ?? String(localized: "no project"), systemImage: "folder").font(.callout).foregroundStyle(.secondary)
                if node.isOrchestrator { Label("Orchestrator", systemImage: "sparkle").font(.callout).foregroundStyle(.secondary) }
            }
            Text(node.displayName).font(.title.weight(.semibold)).textSelection(.enabled)
            HStack(spacing: 8) {
                StatusDot(color: node.dotColor, size: 9)
                Text(node.fleetLabel.capitalizedFirst)
                Text("· \(ago(node.stateSince)) ago").foregroundStyle(.secondary)
            }.font(.callout)
        }
    }
}

private struct ServiceNodeComposer: View {
    let store: StoreOf<NodeDetailFeature>
    let name: String
    @FocusState private var focused: DetailField?
    var body: some View {
        ServiceComposer(placeholder: String(localized: "Write to \(shortName(name))…"),
            text: Binding(get: { store.message }, set: { store.send(.messageChanged($0)) }),
            focus: $focused, sendTitle: String(localized: "Send"), canSend: store.canSend,
            submit: { store.send(.sendTapped) },
            attachmentsStore: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
            .disabled(store.isSendInFlight || store.sendOutcome?.uncertainID != nil)
    }
}

private struct ServiceThreadRow: View {
    let activity: NodeActivity
    let task: WorkTask?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: activity.isMainThread ? "terminal" : "person.2")
                .foregroundStyle(.secondary)
                .frame(width: 18)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 4) {
                    Text(activity.thread)
                        .fontWeight(.medium)
                    if !activity.isMainThread, let task {
                        Text("· subagent of “\(task.title)”")
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                if let doing = activity.doing {
                    Text(doing)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                        .lineLimit(2)
                }
            }
            Spacer()
            Text("\(ago(activity.since)) ago")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
    }
}

