import AlondraShared
import SwiftUI

struct FleetTab: View {
    @Environment(FleetStore.self) private var fleet
    @State private var query = ""
    @State private var showSleeping = false
    @State private var closing: CloseRequest?

    private var sections: [(name: String, nodes: [Node])] {
        let needle = query.trimmingCharacters(in: .whitespaces)
        return fleet.projectsByRecency.compactMap { project in
            let nodes = project.nodes.filter { node in
                (showSleeping || node.alive || !fleet.pending(for: node.sessionId).isEmpty)
                    && (needle.isEmpty || matches(node, project.name, needle))
            }
            return nodes.isEmpty ? nil : (project.name, nodes)
        }
    }

    private func matches(_ node: Node, _ project: String, _ needle: String) -> Bool {
        [node.displayName, project, node.summary, node.cwd ?? "", node.stateLabel]
            .contains { $0.localizedStandardContains(needle) }
    }

    var body: some View {
        NavigationStack {
            List {
                ForEach(sections, id: \.name) { section in
                    Section {
                        ForEach(section.nodes) { node in
                            NavigationLink(value: node.sessionId) {
                                NodeRow(node: node, pending: fleet.pending(for: node.sessionId).count)
                            }
                            .listRowBackground(Palette.card.opacity(0.85))
                            .swipeActions(edge: .trailing) {
                                Button {
                                    closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .node)
                                } label: {
                                    Label("Close Node", systemImage: "xmark.bin")
                                }
                                .tint(Palette.rose)
                            }
                            .contextMenu {
                                Button(role: .destructive) {
                                    closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .node)
                                } label: {
                                    Label("Close Node", systemImage: "xmark.bin")
                                }
                            }
                        }
                    } header: {
                        ProjectHeader(name: section.name, nodes: section.nodes)
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(DawnBackground(glow: Palette.sky))
            .overlay {
                if sections.isEmpty, fleet.overview != nil {
                    ContentUnavailableView.search(text: query)
                }
            }
            .animation(.smooth, value: sections.map(\.name))
            .navigationTitle("Fleet")
            .searchable(text: $query, prompt: "Node, project or what it’s doing")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Toggle(isOn: $showSleeping.animation(.smooth)) {
                            Label("Show Sleeping Nodes", systemImage: "moon.zzz")
                        }
                    } label: {
                        Label("Filter", systemImage: showSleeping ? "line.3.horizontal.decrease.circle.fill" : "line.3.horizontal.decrease.circle")
                    }
                }
            }
            .navigationDestination(for: String.self) { session in
                NodeDetail(session: session)
            }
            .refreshable { await fleet.refresh() }
            .closeConfirmation($closing) { request in
                Task { await fleet.close(request) }
            }
        }
    }
}

private struct ProjectHeader: View {
    let name: String
    let nodes: [Node]

    var body: some View {
        HStack(spacing: 8) {
            Text(name)
                .font(.display(.title3, weight: .bold))
                .foregroundStyle(Palette.ink)
                .textCase(nil)
            Spacer()
            let working = nodes.filter { $0.alive && $0.state == .working }.count
            if working > 0 {
                Label(String(working), systemImage: "hammer.fill")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Palette.sky)
            }
            let waiting = nodes.filter { $0.alive && ($0.state == .waitingAnswer || $0.state == .yourTurn) }.count
            if waiting > 0 {
                Label(String(waiting), systemImage: "hand.raised.fill")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(Palette.dawn)
            }
        }
        .padding(.top, 6)
    }
}

struct NodeRow: View {
    let node: Node
    let pending: Int

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            StateGlyph(node: node)
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    Text(node.displayName)
                        .font(.mono(.subheadline, weight: .semibold))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if node.isOrchestrator {
                        Image(systemName: "point.3.connected.trianglepath.dotted")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(Palette.ember)
                            .accessibilityLabel("Orchestrator")
                    }
                }
                HStack(spacing: 4) {
                    Text(node.alive ? node.stateLabel : String(localized: "asleep · \(node.stateLabel)"))
                        .foregroundStyle(node.alive ? node.state.tint : .secondary)
                    Text(verbatim: "· \(relative(node.stateSince))")
                        .foregroundStyle(.tertiary)
                }
                .font(.caption.weight(.semibold))
                .lineLimit(1)
                if !oneLine.isEmpty {
                    Text(oneLine)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            if pending > 0 {
                Text(pending, format: .number)
                    .font(.caption.weight(.bold).monospacedDigit())
                    .foregroundStyle(Palette.onAccent)
                    .frame(minWidth: 22, minHeight: 22)
                    .background(Palette.dawn, in: .capsule)
            }
        }
        .padding(.vertical, 4)
        .opacity(node.alive ? 1 : 0.6)
    }

    private var oneLine: String {
        let text = node.daemonNeeds ?? node.summary
        return String(MarkdownBlock.inline(text).characters)
            .replacingOccurrences(of: "\n", with: " ")
    }
}

struct StateGlyph: View {
    let node: Node

    var body: some View {
        Image(systemName: node.state.symbol)
            .font(.footnote.weight(.bold))
            .foregroundStyle(node.alive ? node.state.tint : .secondary)
            .frame(width: 34, height: 34)
            .background((node.alive ? node.state.tint : Palette.slate).opacity(0.15), in: .circle)
            .symbolEffect(.pulse, options: .repeating, isActive: node.alive && node.state == .working)
    }
}

struct NodeDetail: View {
    let session: String

    @Environment(FleetStore.self) private var fleet
    @Environment(\.dismiss) private var dismiss
    @State private var closing: CloseRequest?
    @State private var message = ""
    @State private var sending = false
    @State private var sentCount = 0
    @FocusState private var focused: Bool

    var body: some View {
        Group {
            if let node = fleet.node(session) {
                content(node)
            } else {
                ContentUnavailableView("This node is gone", systemImage: "questionmark.circle")
            }
        }
        .background(DawnBackground(glow: fleet.node(session)?.state.tint ?? Palette.dawn))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .toolbar {
            if let node = fleet.node(session) {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button(role: .destructive) {
                            closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .node)
                        } label: {
                            Label("Close Node", systemImage: "xmark.bin")
                        }
                    } label: {
                        Label("More", systemImage: "ellipsis")
                    }
                }
            }
        }
        .closeConfirmation($closing) { request in
            Task {
                if await fleet.close(request) { dismiss() }
            }
        }
        .sensoryFeedback(.success, trigger: sentCount)
    }

    private func content(_ node: Node) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 10) {
                        StateGlyph(node: node)
                        Text(node.alive ? node.stateLabel.prefix(1).uppercased() + node.stateLabel.dropFirst() : String(localized: "Asleep"))
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(node.state.tint)
                        Text("since \(relative(node.stateSince))")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    Text(node.displayName)
                        .font(.display(.largeTitle, weight: .bold))
                        .fixedSize(horizontal: false, vertical: true)
                    if let cwd = node.cwd {
                        Text(cwd.replacing(/^\/Users\/[^\/]+\//, with: "~/"))
                            .font(.mono(.caption))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                    if let tool = node.tool {
                        Label("\(tool.name) \(tool.summary ?? "")" as String, systemImage: "wrench.and.screwdriver")
                            .font(.mono(.footnote))
                            .foregroundStyle(Palette.sky)
                            .lineLimit(2)
                    }
                }

                if let needs = node.daemonNeeds {
                    section(String(localized: "Needs"), symbol: "hand.raised") {
                        Text(needs).font(.body)
                    }
                }

                let pending = fleet.pending(for: session)
                if !pending.isEmpty {
                    section(String(localized: "Waiting for You"), symbol: "tray.full") {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(pending) { item in
                                Button { fleet.focus(item) } label: {
                                    HStack(spacing: 10) {
                                        KindBadge(kind: Kind(item))
                                        Text(item.title)
                                            .font(.subheadline.weight(.medium))
                                            .foregroundStyle(Palette.ink)
                                            .lineLimit(2)
                                            .multilineTextAlignment(.leading)
                                        Spacer(minLength: 0)
                                        Image(systemName: "chevron.right")
                                            .font(.caption.weight(.bold))
                                            .foregroundStyle(.tertiary)
                                    }
                                    .contentShape(.rect)
                                }
                                .buttonStyle(PressStyle())
                                .accessibilityHint("Opens in Waiting for You")
                            }
                        }
                    }
                }

                if let last = node.lastMessage, !last.isEmpty {
                    section(String(localized: "What it said last"), symbol: "text.quote", trailing: node.lastMessageAt.map(relative)) {
                        VStack(alignment: .leading, spacing: 10) {
                            ForEach(MarkdownParser.blocks(from: last)) { block in
                                MarkdownBlockView(block: block)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                }
            }
            .padding(20)
        }
        .scrollDismissesKeyboard(.interactively)
        .safeAreaBar(edge: .bottom) { composer(node) }
    }

    private func section<Content: View>(_ title: String, symbol: String, trailing: String? = nil, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(title, systemImage: symbol)
                    .font(.footnote.weight(.bold))
                    .textCase(.uppercase)
                    .kerning(0.6)
                    .foregroundStyle(.secondary)
                Spacer()
                if let trailing {
                    Text(trailing).font(.caption).foregroundStyle(.tertiary)
                }
            }
            content()
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
                .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 20, style: .continuous))
        }
    }

    private func composer(_ node: Node) -> some View {
        HStack(alignment: .bottom, spacing: 10) {
            TextField("Write to \(node.displayName)…", text: $message, axis: .vertical)
                .lineLimit(1...5)
                .focused($focused)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .glassEffect(.regular.interactive(), in: .rect(cornerRadius: 22))
            Button {
                send(to: node)
            } label: {
                Image(systemName: sending ? "ellipsis" : "arrow.up")
                    .font(.body.weight(.bold))
                    .frame(width: 26, height: 26)
                    .symbolEffect(.variableColor.iterative, isActive: sending)
            }
            .buttonStyle(.glassProminent)
            .buttonBorderShape(.circle)
            .tint(Palette.dawn)
            .disabled(message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || sending)
            .accessibilityLabel("Send")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private func send(to node: Node) {
        let text = message.trimmingCharacters(in: .whitespacesAndNewlines)
        let session = node.sessionId
        sending = true
        Task {
            let ok = await fleet.perform { try await $0.send(to: session, text: text) }
            sending = false
            if ok {
                message = ""
                focused = false
                sentCount += 1
                fleet.notice = Notice(String(localized: "Message sent to \(node.displayName)"))
            }
        }
    }
}

extension FleetStore {
    @discardableResult
    func close(_ request: CloseRequest) async -> Bool {
        let ok = await perform { try await $0.closeNode(request.session) }
        if ok { notice = Notice(String(localized: "\(request.name) closed")) }
        return ok
    }

    var projectsByRecency: [(name: String, nodes: [Node])] {
        Dictionary(grouping: nodes) { $0.projectName ?? String(localized: "No project") }
            .map { (name: $0.key, nodes: $0.value.sorted(by: Self.recencyOrder)) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    private static func recencyOrder(_ a: Node, _ b: Node) -> Bool {
        if a.alive != b.alive { return a.alive }
        return a.stateSince > b.stateSince
    }
}
