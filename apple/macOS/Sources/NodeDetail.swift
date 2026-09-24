import AlondraShared
import SwiftUI

struct NodeDetail: View {
    let node: Node
    let store: FleetStore
    let drafts: Drafts
    let open: (String) -> Void
    let openTask: (String) -> Void
    @FocusState private var focus: DetailField?
    @State private var closing: CloseRequest?

    var body: some View {
        let draft = drafts.draft(for: "n\(node.sessionId)")
        let items = store.items(for: node.sessionId)
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                header
                if !items.isEmpty {
                    section(String(localized: "Waiting for You")) {
                        VStack(alignment: .leading, spacing: 2) {
                            ForEach(items) { item in
                                Button { open(item.id) } label: {
                                    HStack {
                                        InboxRow(item: item)
                                        Spacer()
                                        Image(systemName: "chevron.right")
                                            .font(.caption.weight(.semibold))
                                            .foregroundStyle(.tertiary)
                                    }
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 4)
                                    .contentShape(.rect)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                    }
                }
                let tasks = node.isOrchestrator ? [] : store.board.tasks(onNode: node.sessionId)
                if !tasks.isEmpty {
                    section(String(localized: "Tasks")) {
                        VStack(alignment: .leading, spacing: 2) {
                            ForEach(tasks) { task in
                                Button { openTask(task.id) } label: {
                                    HStack(spacing: 10) {
                                        StatusDot(color: task.color)
                                        VStack(alignment: .leading, spacing: 1) {
                                            Text(task.title)
                                                .fontWeight(.medium)
                                                .strikethrough(task.state == .cancelled)
                                            Text(verbatim: taskLine(task))
                                                .font(.caption)
                                                .foregroundStyle(.secondary)
                                        }
                                        Spacer()
                                        Image(systemName: "chevron.right")
                                            .font(.caption.weight(.semibold))
                                            .foregroundStyle(.tertiary)
                                    }
                                    .padding(.horizontal, 12)
                                    .padding(.vertical, 7)
                                    .contentShape(.rect)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                    }
                }
                let threads = store.activity(of: node)
                if !threads.isEmpty {
                    section(String(localized: "Right now")) {
                        VStack(alignment: .leading, spacing: 2) {
                            ForEach(Array(threads.enumerated()), id: \.offset) { _, thread in
                                ThreadRow(activity: thread, task: thread.task.flatMap(store.board.task))
                            }
                        }
                        .background(Color.primary.opacity(0.04), in: .rect(cornerRadius: 10))
                    }
                }
                if !node.running.isEmpty {
                    section(BackgroundItem.sectionTitle) {
                        VStack(alignment: .leading, spacing: 6) {
                            ForEach(node.running, id: \.self) { item in
                                BackgroundRow(item: item)
                            }
                        }
                    }
                }
                if let needs = node.daemonNeeds, !needs.isEmpty {
                    section(String(localized: "What it needs")) {
                        Text(needs).textSelection(.enabled)
                    }
                }
                if let last = node.lastMessage ?? node.daemonDetail, !last.isEmpty {
                    section(node.lastMessageAt.map { String(localized: "What it said last · \(ago($0)) ago") } ?? String(localized: "What it said last")) {
                        MarkdownText(text: last)
                    }
                }
                if let cwd = node.cwd {
                    Label(cwd, systemImage: "folder")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                        .textSelection(.enabled)
                }
            }
        } bar: {
            NodeComposer(node: node, store: store, draft: draft, focus: $focus)
        }
        .toolbar {
            ToolbarItem {
                Button(role: .destructive) {
                    closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .node)
                } label: {
                    Label("Close Node…", systemImage: "xmark.bin")
                }
                .help("Close this node with claude rm")
            }
        }
        .closeConfirmation($closing) { _ in
            let store = store
            let node = node
            Task { await store.close(node) }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Label(node.projectName ?? String(localized: "no project"), systemImage: "folder")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                if node.isOrchestrator {
                    Label("Orchestrator", systemImage: "sparkle")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }
            Text(node.displayName)
                .font(.title.weight(.semibold))
                .textSelection(.enabled)
            HStack(spacing: 8) {
                StatusDot(color: node.dotColor, size: 9)
                Text(node.fleetLabel.capitalizedFirst)
                Text("· \(ago(node.stateSince)) ago")
                    .foregroundStyle(.secondary)
            }
            .font(.callout)
        }
    }

    private func taskLine(_ task: WorkTask) -> String {
        guard let request = store.request(of: task) else { return task.state.label }
        return "\(task.state.label) · \(String(localized: "from «\(store.title(of: request))»"))"
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            content()
        }
    }
}

/// One thread of a node, the main one or a subagent, and what it does now.
private struct ThreadRow: View {
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
                        Text("· subagent of «\(task.title)»")
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

private struct NodeComposer: View {
    let node: Node
    let store: FleetStore
    @Bindable var draft: ReviewDraft
    var focus: FocusState<DetailField?>.Binding

    var body: some View {
        Composer(
            placeholder: String(localized: "Write to \(shortName(node.displayName))…"),
            text: $draft.general,
            focus: focus,
            sendTitle: String(localized: "Send"),
            canSend: !draft.general.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !draft.attachments.isEmpty,
            send: send,
            attachments: draft.attachments,
            client: store.client
        )
    }

    private func send() {
        let text = draft.general.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty || !draft.attachments.isEmpty else { return }
        draft.general = ""
        let attachments = draft.attachments
        draft.attachments = Attachments()
        let store = store
        let node = node
        Task { await store.send(to: node, text: text, attachments: attachments) }
    }
}

extension String {
    var capitalizedFirst: String {
        prefix(1).uppercased() + dropFirst()
    }
}
