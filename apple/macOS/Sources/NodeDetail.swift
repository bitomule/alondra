import AlondraShared
import SwiftUI

struct NodeDetail: View {
    let node: Node
    let store: FleetStore
    let drafts: Drafts
    let open: (String) -> Void
    @FocusState private var focus: DetailField?

    var body: some View {
        let draft = drafts.draft(for: "n\(node.sessionId)")
        let items = store.items(for: node.sessionId)
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                header
                if !items.isEmpty {
                    section("Te espera") {
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
                if let tool = node.tool {
                    section("Ahora mismo") {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Image(systemName: "hammer")
                                .foregroundStyle(.secondary)
                            Text("\(tool.name) \(tool.summary ?? "")")
                                .font(.system(.callout, design: .monospaced))
                                .textSelection(.enabled)
                            Spacer()
                            Text("hace \(ago(tool.since))")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                if let needs = node.daemonNeeds, !needs.isEmpty {
                    section("Qué necesita") {
                        Text(needs).textSelection(.enabled)
                    }
                }
                if let last = node.lastMessage ?? node.daemonDetail, !last.isEmpty {
                    section(node.lastMessageAt.map { "Lo último que dijo · hace \(ago($0))" } ?? "Lo último que dijo") {
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
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Label(node.project ?? "sin proyecto", systemImage: "folder")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                if node.isOrchestrator {
                    Label("Orquestador", systemImage: "sparkle")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }
            Text(node.displayName)
                .font(.title.weight(.semibold))
                .textSelection(.enabled)
            HStack(spacing: 8) {
                StatusDot(state: node.state, alive: node.alive, size: 9)
                Text(node.state.label.capitalizedFirst)
                Text("· hace \(ago(node.stateSince))")
                    .foregroundStyle(.secondary)
                if !node.alive {
                    Label("Dormido", systemImage: "moon.zzz")
                        .foregroundStyle(.secondary)
                }
            }
            .font(.callout)
        }
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

private struct NodeComposer: View {
    let node: Node
    let store: FleetStore
    @Bindable var draft: ReviewDraft
    var focus: FocusState<DetailField?>.Binding

    var body: some View {
        Composer(
            placeholder: node.alive ? "Escribe a \(shortName(node.displayName))…" : "Escribe a \(shortName(node.displayName)) (lo despierta)…",
            text: $draft.general,
            focus: focus,
            sendTitle: "Enviar",
            canSend: !draft.general.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
            send: send
        )
    }

    private func send() {
        let text = draft.general.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        draft.general = ""
        let store = store
        let node = node
        Task { await store.send(to: node, text: text) }
    }
}

extension String {
    var capitalizedFirst: String {
        prefix(1).uppercased() + dropFirst()
    }
}
